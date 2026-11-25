//! [`MoneyModel`]: what an engine counts money in, the contract KIND (linear or inverse, decision
//! 0170) and the coin it SETTLES in, and the one rule of which other contract an engine may also
//! hold ([`MoneyModel::admits`]): the same kind (decision 0233 item 5: one engine counts money one
//! way) and the same settle coin (decision 0258 item 7: the venue keeps one wallet per coin).
//!
//! The answer is TYPED ([`MoneyModelRefusal`]), never words: each door and each bridge keeps its own
//! sentence, so a journaled refusal reads byte for byte as it did. Both refusals hold on a LINEAR
//! engine too (the owner's ruling of 2026-11-22, decision 0258 item 7): a `USDT` engine refuses a
//! linear contract settled in `USDC`, its PnL being another wallet's.

use crate::instrument::contract::Contract;
use crate::money::contract_value::{ContractValue, NotionalKind};
use crate::money::currency::{currency_code, same_currency};

/// **What an engine counts money in**: the kind of its contracts and the coin they settle in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoneyModel {
    /// Linear or inverse.
    pub kind: NotionalKind,
    /// The settle coin, in [`currency_code`]'s spelling (`BTC`, `USDC`).
    pub settle: String,
}

/// **Why one money model does not admit another** ([`MoneyModel::admits`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoneyModelRefusal {
    /// The other contract counts money the other way: a linear one beside an inverse engine's, or
    /// the reverse.
    Kind,
    /// The same kind, another settle coin: another venue wallet.
    SettleCoin,
}

impl MoneyModel {
    /// The model of `contract`: its kind ([`ContractValue::of`]'s, so an option is linear) and its
    /// settle asset. `None` for a contract this build does not value (a quanto) and for one that
    /// names no settle coin (a cash class, a combo the venue published none for).
    #[must_use]
    pub fn of_contract(contract: &Contract) -> Option<Self> {
        let kind = ContractValue::of(contract, 1.0)?.kind;
        Some(Self { kind, settle: currency_code(contract.settle_asset()?)? })
    }

    /// Whether an engine of this model may also hold a contract of `other`: the same kind first,
    /// then the same settle coin ([`same_currency`]).
    ///
    /// # Errors
    /// [`MoneyModelRefusal::Kind`] for the other kind, [`MoneyModelRefusal::SettleCoin`] for the
    /// same kind in another coin.
    pub fn admits(&self, other: &MoneyModel) -> Result<(), MoneyModelRefusal> {
        if self.kind != other.kind {
            Err(MoneyModelRefusal::Kind)
        } else if !same_currency(&self.settle, &other.settle) {
            Err(MoneyModelRefusal::SettleCoin)
        } else {
            Ok(())
        }
    }
}

#[path = "money_model_tests.rs"]
#[cfg(test)]
mod money_model_tests;
