//! **What the venue confirmed an account may margin a coin at, kept for the ADAPTER's lifetime**
//! (decision 0181, v0.1.71 review finding 7).
//!
//! `TradeLane::leverage_cap` is asked at every `AdapterStart`, including one that finds the adapter
//! already running (`session.rs`'s `start_adapter_locked` re-asks "afresh" so a row set after the adapter
//! came up is honoured). Against Hyperliquid each ask is up to three `/info` reads (~100 weight of the
//! per-IP budget), so a flapping tradehub link spent that on every reconnect for a figure that had not
//! changed. A confirmed cap is therefore kept under `(account, tier, coin, the stated leverage)`:
//!
//! * a re-ask with the same stated leverage reads NOTHING;
//! * a changed stated leverage is a different key, so it is read afresh (the row was edited);
//! * [`LeverageCaps::forget`] drops an account's entries when its adapter is (re)started, so the next
//!   ask after a restart reads the venue again;
//! * only a CONFIRMED figure is kept. A refusal or a failed read is not (it may be a network blip or a
//!   leverage the operator is about to fix on the venue), so it is asked again next time.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use vike_model::AccountTier;

/// What a kept figure answers for.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    /// The wire account text (`""` = the default account).
    account: String,
    tier: AccountTier,
    coin: String,
    /// The stated leverage's bits (`f64` is no key; the figure is never `NaN`).
    stated: u64,
}

/// See the module doc. One per lane, shared by every generation of it.
#[derive(Default)]
pub struct LeverageCaps {
    held: Mutex<HashMap<Key, f64>>,
}

impl LeverageCaps {
    /// The kept cap for `(account, tier, coin, stated)`, else `read` (no lock held across it) and, when it
    /// confirms a figure, keep that.
    ///
    /// # Errors
    /// `read`'s own refusal, unchanged and not kept.
    pub fn confirmed(
        &self,
        account: &str,
        tier: AccountTier,
        coin: &str,
        stated: f64,
        read: impl FnOnce() -> Result<f64, String>,
    ) -> Result<f64, String> {
        let key = Key {
            account: account.to_string(),
            tier,
            coin: coin.to_string(),
            stated: stated.to_bits(),
        };
        if let Some(cap) = self.held.lock().unwrap_or_else(PoisonError::into_inner).get(&key) {
            return Ok(*cap);
        }
        let cap = read()?;
        self.held.lock().unwrap_or_else(PoisonError::into_inner).insert(key, cap);
        Ok(cap)
    }

    /// Drop every figure kept for `account`: its adapter is being (re)started.
    pub fn forget(&self, account: &str) {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|key, _| key.account != account);
    }
}

#[path = "leverage_caps_tests.rs"]
#[cfg(test)]
mod leverage_caps_tests;
