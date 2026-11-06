//! Hyperliquid credential loading + the live gate, plus the small shared enums the rest of the
//! crate keys off (`Network`/`Product`).
//!
//! Bespoke credential shape (like Polymarket/FX, not the standard API_KEY/SECRET): a secp256k1
//! **private key** (the SIGNER) + an optional master **account address** — the started ACCOUNT's
//! fields `PRIVATE_KEY` (+ optional `ACCOUNT_ADDRESS`), read out of the
//! [`vike_secrets::account_credentials::AccountCredentials`] the start is handed (decision 0156
//! item 6: a credential belongs to its account, and no name is composed here).
//! **Absent key ⇒ `None` ⇒ the venue stays paper** (the live gate). If `ACCOUNT_ADDRESS` is
//! present the key is an **agent (API) wallet** signing for that master account (all `/info` reads
//! use the master); if absent, the key *is* the account (its address is derived from the key).
//! The account's tier picks the network by a `match` on the tier itself (decision 0156: the bridge
//! keeps no tier enum of its own): `live` is mainnet, every tier below it testnet
//! ([`Network::for_tier`]); the account's key is the same field at either.
//!
//! The private key is a secret: [`HlCredentials`] has a manual redacting `Debug`, and the key must
//! never reach argv/logs (pass via env to any child, like the other venues).

use vike_model::AccountTier;
use vike_secrets::account_credentials::AccountCredentials;

/// The account field holding the secp256k1 private key — the SIGNER. Required: its absence is the
/// live gate. Both field words are `vike_venues::credential_schema`'s hyperliquid row's, held to it
/// by this module's `the_fields_are_the_schemas`.
pub(crate) const PRIVATE_KEY: &str = "PRIVATE_KEY";
/// The account field holding the MASTER account address the key signs for — optional (absent ⇒ the
/// key IS the account).
pub(crate) const ACCOUNT_ADDRESS: &str = "ACCOUNT_ADDRESS";

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

impl Network {
    /// The network an account's tier reaches (decision 0095, item 3 of 0119): `live` means
    /// MAINNET, every tier below it testnet — [`AccountTier::Paper`], which never reaches a bridge
    /// (`vike-tradehub-core` returns first), reads as `demo` does (decision 0129 item 5). The one mapping
    /// this bridge's `resolve`, `start` and clock read share. (The daemon's feed plan used to call
    /// it too; it asks the venue's registry row for the tier now, so it cannot disagree with the
    /// account it serves.)
    #[must_use]
    pub fn for_tier(tier: AccountTier) -> Network {
        match tier {
            AccountTier::Live => Network::Mainnet,
            AccountTier::Demo | AccountTier::Paper => Network::Testnet,
        }
    }

    /// The phantom-agent `source`: `"a"` mainnet, `"b"` testnet. Part of the signed L1 payload — a
    /// wrong value silently rejects.
    pub fn phantom_source(self) -> &'static str {
        match self {
            Network::Mainnet => "a",
            Network::Testnet => "b",
        }
    }
    /// The network as an operator says it (`mainnet` / `testnet`), for a refusal that names whose
    /// universe an order was judged against.
    pub fn word(self) -> &'static str {
        match self {
            Network::Mainnet => "mainnet",
            Network::Testnet => "testnet",
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
                crate::base::consts::MAINNET_INFO,
                crate::base::consts::MAINNET_EXCHANGE,
                crate::base::consts::MAINNET_WS,
            ),
            Network::Testnet => (
                crate::base::consts::TESTNET_INFO,
                crate::base::consts::TESTNET_EXCHANGE,
                crate::base::consts::TESTNET_WS,
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

/// Load the started ACCOUNT's credentials — its `PRIVATE_KEY` and optional `ACCOUNT_ADDRESS`
/// fields — for the network `tier` selects ([`Network::for_tier`]). `None` (the live gate) when the private key is
/// absent/blank. Addresses are lowercased (HL requires lowercased address fields for signing) and
/// `0x`-normalized is left to the signer.
///
/// The fields carry no tier: the account HAS one (`account.tier`, the `tier` argument), so a `live` account's key signs on mainnet and a `demo`
/// account's on testnet. There is no fallback from one account's fields to another's: a labelled
/// account holding no key reads as ABSENT, and its MASTER ADDRESS is its own too — an agent wallet
/// plus somebody else's master address would be a signer trading a DIFFERENT account.
pub fn load(tier: AccountTier, creds: &AccountCredentials<'_>) -> Option<HlCredentials> {
    let private_key = creds.get(PRIVATE_KEY)?.to_string();
    let account_address =
        creds.get(ACCOUNT_ADDRESS).map(str::to_ascii_lowercase).filter(|s| !s.is_empty());
    Some(HlCredentials { private_key, account_address, network: Network::for_tier(tier) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_secrets::account_credentials::CredentialFields;

    /// The account holding `fields`, as a start is handed it.
    fn holding(fields: &CredentialFields) -> AccountCredentials<'_> {
        AccountCredentials::new(Some(1), Some(fields), None)
    }

    #[test]
    fn absent_key_is_the_live_gate() {
        for tier in [AccountTier::Demo, AccountTier::Live] {
            assert!(load(tier, &AccountCredentials::none()).is_none());
            let address_only =
                CredentialFields::from_pairs([(ACCOUNT_ADDRESS, "0x8f0a3e01d916486735a8f6a2ffc0")]);
            assert!(load(tier, &holding(&address_only)).is_none(), "no key, no credentials");
            let blank_key = CredentialFields::from_pairs([(PRIVATE_KEY, "   ")]);
            assert!(load(tier, &holding(&blank_key)).is_none(), "a blank key is absent");
        }
    }

    /// **`paper` answers exactly as `demo`** (decision 0129 item 5): the account's key, testnet —
    /// never mainnet. `vike-tradehub-core` never hands a bridge `paper`; this pins that answer.
    #[test]
    fn paper_answers_as_demo_and_never_mainnet() {
        assert_eq!(Network::for_tier(AccountTier::Paper), Network::Testnet);
        assert_eq!(Network::for_tier(AccountTier::Demo), Network::Testnet);
        assert_eq!(Network::for_tier(AccountTier::Live), Network::Mainnet);
        let fields = CredentialFields::from_pairs([(PRIVATE_KEY, "0xabc123")]);
        let c = load(AccountTier::Paper, &holding(&fields)).expect("the account's key");
        assert_eq!(c.network, Network::Testnet);
    }

    #[test]
    fn demo_loads_testnet_with_optional_master_address() {
        let fields = CredentialFields::from_pairs([
            (PRIVATE_KEY, "0xabc123"),
            (ACCOUNT_ADDRESS, "0x8F0A3E01D916486735A8F6A2FFC0685A3FA57BF5"),
        ]);
        let c = load(AccountTier::Demo, &holding(&fields)).expect("loads");
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
        let fields = CredentialFields::from_pairs([(PRIVATE_KEY, "0xdeadbeef")]);
        let c = load(AccountTier::Live, &holding(&fields)).expect("loads");
        assert_eq!(c.network, Network::Mainnet);
        assert!(c.account_address.is_none());
    }

    /// **The fields the loader reads are `vike_venues::credential_schema`'s hyperliquid row, word
    /// for word**: [`PRIVATE_KEY`] and [`ACCOUNT_ADDRESS`] are exactly the row's ACCOUNT fields, the
    /// key is the one required field, and an account holding exactly the required fields
    /// configures the loader at either network.
    #[test]
    fn the_fields_are_the_schemas() {
        use std::collections::BTreeSet;
        use vike_venues::credential_schema::{FieldOwner, schema_for};
        let schema = schema_for(crate::base::consts::VENUE).expect("hyperliquid is a schema row");
        let ours: BTreeSet<&str> = [PRIVATE_KEY, ACCOUNT_ADDRESS].into_iter().collect();
        let theirs: BTreeSet<&str> = schema.account_fields().map(|f| f.field).collect();
        assert_eq!(ours, theirs, "the loader's fields are the schema row's account fields");
        assert!(schema.venue_fields().next().is_none(), "hyperliquid holds no venue field");
        let required = schema.required(FieldOwner::Account);
        assert_eq!(required, [PRIVATE_KEY], "the loader refuses without exactly the key");
        let only_required = CredentialFields::from_pairs(required.iter().map(|f| (*f, "0xabc123")));
        for tier in [AccountTier::Demo, AccountTier::Live] {
            assert!(
                load(tier, &holding(&only_required)).is_some(),
                "{tier:?}: the schema's required fields must configure the loader"
            );
        }
    }
}
