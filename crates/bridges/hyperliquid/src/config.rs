//! Hyperliquid credential loading + the live gate, plus the small shared enums the rest of the
//! crate keys off (`Env`/`Network`/`Product`).
//!
//! Bespoke credential shape (like Polymarket/FX, not the standard API_KEY/SECRET): a secp256k1
//! **private key** (the SIGNER) + an optional master **account address**. Naming:
//! `HYPERLIQUID_{DEMO|LIVE}_PRIVATE_KEY` (+ optional `_ACCOUNT_ADDRESS`), read from the gitignored
//! workspace `.env` via [`vike_bridge_core::credentials::load_workspace_dotenv`]. **Absent key ⇒
//! `None` ⇒ the venue stays paper** (the live gate). If `ACCOUNT_ADDRESS` is present the key is an
//! **agent (API) wallet** signing for that master account (all `/info` reads use the master); if
//! absent, the key *is* the account (its address is derived from the key). `DEMO` ⇒ testnet.
//!
//! The private key is a secret: [`HlCredentials`] has a manual redacting `Debug`, and the key must
//! never reach argv/logs (pass via env to any child, like the other venues).

use std::collections::HashMap;

use vike_bridge_core::credentials::account_var;
use vike_model::account_keys::AccountLabel;

/// Credential tier — selects the env-var prefix and the network. Mirrors the workspace
/// `SIM|DEMO|LIVE` convention (HL has no separate SIM tier; DEMO = testnet).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Env {
    Demo,
    Live,
}

/// Which Hyperliquid deployment — drives host selection, the phantom-agent `source` byte, and (for
/// deferred user-signed actions) the `hyperliquidChain` field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Network {
    Testnet,
    Mainnet,
}

/// Spot vs perpetual — the one axis along which the otherwise-shared bridge branches (asset-id
/// math, symbology, the balance endpoint, the fills channel).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Product {
    Perp,
    Spot,
}

impl Env {
    /// The `.env` var prefix for this tier.
    pub fn prefix(self) -> &'static str {
        match self {
            Env::Demo => "HYPERLIQUID_DEMO",
            Env::Live => "HYPERLIQUID_LIVE",
        }
    }
    /// DEMO ⇒ testnet, LIVE ⇒ mainnet.
    pub fn network(self) -> Network {
        match self {
            Env::Demo => Network::Testnet,
            Env::Live => Network::Mainnet,
        }
    }
}

impl Network {
    /// The phantom-agent `source`: `"a"` mainnet, `"b"` testnet. Part of the signed L1 payload — a
    /// wrong value silently rejects.
    pub fn phantom_source(self) -> &'static str {
        match self {
            Network::Mainnet => "a",
            Network::Testnet => "b",
        }
    }
    /// The `hyperliquidChain` field for user-signed actions (deferred).
    pub fn hyperliquid_chain(self) -> &'static str {
        match self {
            Network::Mainnet => "Mainnet",
            Network::Testnet => "Testnet",
        }
    }
    /// `(info_url, exchange_url, ws_url)` for this network.
    pub fn urls(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Network::Mainnet => (
                crate::consts::MAINNET_INFO,
                crate::consts::MAINNET_EXCHANGE,
                crate::consts::MAINNET_WS,
            ),
            Network::Testnet => (
                crate::consts::TESTNET_INFO,
                crate::consts::TESTNET_EXCHANGE,
                crate::consts::TESTNET_WS,
            ),
        }
    }
}

/// Resolved Hyperliquid credentials. `private_key` is the signer (agent or master); redacted in
/// `Debug`. `account_address` is the master address when the key is an agent wallet, else `None`
/// (derive from the key at signer-construction time).
pub struct HlCredentials {
    pub private_key: String,
    pub account_address: Option<String>,
    pub network: Network,
}

impl std::fmt::Debug for HlCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HlCredentials")
            .field("private_key", &"<redacted>")
            .field("account_address", &self.account_address)
            .field("network", &self.network)
            .finish()
    }
}

/// Load the tier's credentials from an already-read `.env` var map. `None` (the live gate) when the
/// private key is absent/blank. Addresses are lowercased (HL requires lowercased address fields for
/// signing) and `0x`-normalized is left to the signer.
pub fn load(env: Env, vars: &HashMap<String, String>) -> Option<HlCredentials> {
    load_for_account(env, &AccountLabel::Default, vars)
}

/// [`load`] for ONE NAMED ACCOUNT — `HYPERLIQUID_{TIER}_{SUFFIX}__{LABEL}`.
///
/// Everything [`load`] documents holds word for word; the ONLY difference is the NAMES read, and
/// they are composed by `vike_bridge_core::credentials::account_var`, which appends the label after
/// the WHOLE of today's key. `HYPERLIQUID_LIVE_PRIVATE_KEY` — a two-word bespoke suffix — therefore
/// needs no entry in any table, and neither does the OPTIONAL `_ACCOUNT_ADDRESS`.
///
/// ⚠ **[`AccountLabel::Default`] is byte-identically [`load`]** — the same function, reached
/// through it — because `vike_model::account_keys::account_key` returns its input unchanged for
/// that account.
///
/// ⚠ **No fallback to the unlabelled key.** A labelled account whose private key is absent reads as
/// ABSENT (the live gate, unchanged); the alternative would sign account `ALT`'s orders with the
/// default account's key. The MASTER ADDRESS is scoped the same way, and that half matters just as
/// much: an agent wallet plus somebody else's master address is a signer trading a DIFFERENT
/// account.
pub fn load_for_account(
    env: Env,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> Option<HlCredentials> {
    let p = env.prefix();
    let private_key = account_var(vars, &format!("{p}_PRIVATE_KEY"), label)?.to_string();
    let account_address = account_var(vars, &format!("{p}_ACCOUNT_ADDRESS"), label)
        .map(str::to_ascii_lowercase)
        .filter(|s| !s.is_empty());
    Some(HlCredentials { private_key, account_address, network: env.network() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_key_is_the_live_gate() {
        let vars = HashMap::new();
        assert!(load(Env::Demo, &vars).is_none());
        assert!(load(Env::Live, &vars).is_none());
    }

    #[test]
    fn demo_loads_testnet_with_optional_master_address() {
        let mut vars = HashMap::new();
        vars.insert("HYPERLIQUID_DEMO_PRIVATE_KEY".into(), "0xabc123".into());
        vars.insert(
            "HYPERLIQUID_DEMO_ACCOUNT_ADDRESS".into(),
            "0x8F0A3E01D916486735A8F6A2FFC0685A3FA57BF5".into(),
        );
        let c = load(Env::Demo, &vars).expect("loads");
        assert_eq!(c.network, Network::Testnet);
        assert_eq!(c.network.phantom_source(), "b");
        // address lowercased for signing
        assert_eq!(
            c.account_address.as_deref(),
            Some("0x8f0a3e01d916486735a8f6a2ffc0685a3fa57bf5")
        );
        // secret never rendered
        assert!(!format!("{c:?}").contains("0xabc123"));
    }

    #[test]
    fn key_without_address_means_key_is_the_account() {
        let mut vars = HashMap::new();
        vars.insert("HYPERLIQUID_LIVE_PRIVATE_KEY".into(), "0xdeadbeef".into());
        let c = load(Env::Live, &vars).expect("loads");
        assert_eq!(c.network, Network::Mainnet);
        assert!(c.account_address.is_none());
    }
}
