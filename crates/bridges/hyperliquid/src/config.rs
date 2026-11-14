//! Hyperliquid credential loading + the live gate, plus the small shared words the rest of the
//! crate keys off (the network functions over `vike_model::VenueNetwork`, and `unlisted_class`: the refusal of a class this venue lists none of).
//!
//! Bespoke credential shape (like Polymarket/FX, not the standard API_KEY/SECRET): a secp256k1
//! **private key** (the SIGNER) + an optional master **account address** — the started ACCOUNT's
//! fields `PRIVATE_KEY` (+ optional `ACCOUNT_ADDRESS`), read out of the
//! [`vike_secrets::account_credentials::AccountCredentials`] the start is handed (decision 0156
//! item 6: a credential belongs to its account, and no name is composed here).
//! **Absent key ⇒ `None` ⇒ the account is NOT RUNNING** (the live gate). If `ACCOUNT_ADDRESS` is
//! present the key is an **agent (API) wallet** signing for that master account (all `/info` reads
//! use the master); if absent, the key *is* the account (its address is derived from the key).
//! The account's tier picks the network by a `match` on the tier itself (decision 0156: the bridge
//! keeps no tier enum of its own): `live` is mainnet, every tier below it testnet
//! (`vike_model::VenueNetwork::for_tier`); the account's key is the same field at either.
//!
//! The private key is a secret: [`HyperliquidConfig`] has a manual redacting `Debug`, and the key must
//! never reach argv/logs (pass via env to any child, like the other venues).

use vike_model::secret::Secret;
use vike_model::{AccountTier, AssetClass, VenueNetwork};
use vike_secrets::account_credentials::AccountCredentials;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

/// The account field holding the secp256k1 private key — the SIGNER. Required: its absence is the
/// live gate. Both field words are `vike_venues::credential_schema`'s hyperliquid row's, held to it
/// by this module's `the_fields_are_the_schemas`.
pub(crate) const PRIVATE_KEY: &str = "PRIVATE_KEY";
/// The account field holding the MASTER account address the key signs for — optional (absent ⇒ the
/// key IS the account).
pub(crate) const ACCOUNT_ADDRESS: &str = "ACCOUNT_ADDRESS";

/// **The words a class this venue lists none of is refused in**, naming the class and the venue. Spot vs
/// perpetual is the one axis along which the otherwise-shared bridge branches (asset-id math, symbology,
/// the balance endpoint, the fills channel), spoken as [`AssetClass::CryptoPerp`] (`meta`) and
/// [`AssetClass::CryptoSpot`] (`spotMeta`); every perp/spot match answers any other class with these.
pub(crate) fn unlisted_class(class: AssetClass) -> String {
    format!(
        "{} lists no {class:?} instrument: its classes are CryptoPerp (meta) and CryptoSpot (spotMeta)",
        crate::transport::consts::VENUE
    )
}

// The network is `vike_model::VenueNetwork` (decision 0164: the tradehub's feed plan names it
// and links no bridge); `VenueNetwork::for_tier` is the one tier mapping this bridge's `resolve`,
// `start` and clock read share. What a network means HERE (its source byte, its chain name, its
// address rows) is the functions below.

/// The phantom-agent `source`: `"a"` mainnet, `"b"` testnet. Part of the signed L1 payload — a
/// wrong value silently rejects.
pub fn phantom_source(network: VenueNetwork) -> &'static str {
    match network {
        VenueNetwork::Mainnet => "a",
        VenueNetwork::Testnet => "b",
    }
}

/// The `hyperliquidChain` field for user-signed actions (deferred).
pub fn hyperliquid_chain(network: VenueNetwork) -> &'static str {
    match network {
        VenueNetwork::Mainnet => "Mainnet",
        VenueNetwork::Testnet => "Testnet",
    }
}

/// The `venue_url` type a network's addresses are: mainnet `live`, the testnet `demo`. The address
/// rows follow the network, and so do the signing identity ([`phantom_source`],
/// [`hyperliquid_chain`]): both come from ONE `VenueNetwork`, never from a url (PLAN trap 2), so a
/// row cannot move a signature onto the other chain.
fn url_type(network: VenueNetwork) -> UrlType {
    match network {
        VenueNetwork::Mainnet => UrlType::Live,
        VenueNetwork::Testnet => UrlType::Demo,
    }
}

/// `network`'s base address out of hyperliquid's rows `urls` for `channel` over `protocol`
/// (`https://api.hyperliquid.xyz` for mainnet REST in the seed): `/info` and `/exchange`
/// ([`crate::transport::consts::INFO_PATH`], [`crate::transport::consts::EXCHANGE_PATH`]) and `/ws`
/// ([`crate::transport::consts::WEBSOCKET_PATH`]) hang off it in code.
///
/// # Errors
/// The row's refusal, whose words are the NOT RUNNING reason.
pub fn base_url(
    network: VenueNetwork,
    urls: &VenueUrls,
    channel: UrlChannel,
    protocol: UrlProtocol,
) -> Result<&str, UrlRefusal> {
    urls.url(url_type(network), channel, protocol, "")
}

/// `network`'s websocket for `channel` (the `ws` row plus [`crate::transport::consts::WEBSOCKET_PATH`]).
///
/// # Errors
/// The row's refusal.
pub fn websocket_url(
    network: VenueNetwork,
    urls: &VenueUrls,
    channel: UrlChannel,
) -> Result<String, UrlRefusal> {
    Ok(format!(
        "{}{}",
        base_url(network, urls, channel, UrlProtocol::Websocket)?,
        crate::transport::consts::WEBSOCKET_PATH
    ))
}

/// **An exec client's two addresses on its network**, looked up once by the start: the REST base its
/// `/info` and `/exchange` hang off (the `trade` row) and its private stream (the `user` ws row plus
/// `/ws`). Both at the network's type: a testnet account never falls back to a live row here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HyperliquidUrls {
    /// The `trade` REST base ([`base_url`]).
    pub rest: String,
    /// The account's private websocket ([`websocket_url`], channel `user`).
    pub user_websocket: String,
}

impl HyperliquidUrls {
    /// `network`'s exec addresses out of hyperliquid's rows `urls`.
    ///
    /// # Errors
    /// The first row refused, whose words are the account's NOT RUNNING reason.
    pub fn of(network: VenueNetwork, urls: &VenueUrls) -> Result<HyperliquidUrls, UrlRefusal> {
        Ok(HyperliquidUrls {
            rest: base_url(network, urls, UrlChannel::Trade, UrlProtocol::Rest)?.to_string(),
            user_websocket: websocket_url(network, urls, UrlChannel::User)?,
        })
    }
}

/// Resolved Hyperliquid credentials. `private_key` is the signer (agent or master), a [`Secret`] the
/// derived `Debug` prints redacted. `account_address` is the master address when the key is an agent
/// wallet, else `None` (derive from the key at signer-construction time).
#[derive(Debug)]
pub struct HyperliquidConfig {
    pub private_key: Secret<String>,
    pub account_address: Option<String>,
    pub network: VenueNetwork,
}

/// Load the started ACCOUNT's credentials — its `PRIVATE_KEY` and optional `ACCOUNT_ADDRESS`
/// fields — for the network `tier` selects (`VenueNetwork::for_tier`). `None` (the live gate) when the private key is
/// absent/blank. Addresses are lowercased (HL requires lowercased address fields for signing) and
/// `0x`-normalized is left to the signer.
///
/// The fields carry no tier: the account HAS one (`account.tier`, the `tier` argument), so a `live` account's key signs on mainnet and a `demo`
/// account's on testnet. There is no fallback from one account's fields to another's: a labelled
/// account holding no key reads as ABSENT, and its MASTER ADDRESS is its own too — an agent wallet
/// plus somebody else's master address would be a signer trading a DIFFERENT account.
pub fn load(tier: AccountTier, creds: &AccountCredentials<'_>) -> Option<HyperliquidConfig> {
    let private_key = Secret::new(creds.get(PRIVATE_KEY)?.to_string());
    let account_address =
        creds.get(ACCOUNT_ADDRESS).map(str::to_ascii_lowercase).filter(|s| !s.is_empty());
    Some(HyperliquidConfig { private_key, account_address, network: VenueNetwork::for_tier(tier) })
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
            let address_only = CredentialFields::from_pairs([(ACCOUNT_ADDRESS, "0xabcdef01")]);
            assert!(load(tier, &holding(&address_only)).is_none(), "no key, no credentials");
            let blank_key = CredentialFields::from_pairs([(PRIVATE_KEY, "   ")]);
            assert!(load(tier, &holding(&blank_key)).is_none(), "a blank key is absent");
        }
    }

    /// **`paper` answers exactly as `demo`** (decision 0129 item 5): the account's key, testnet —
    /// never mainnet. `vike-tradehub-core` never hands a bridge `paper`; this pins that answer.
    #[test]
    fn paper_answers_as_demo_and_never_mainnet() {
        assert_eq!(VenueNetwork::for_tier(AccountTier::Paper), VenueNetwork::Testnet);
        assert_eq!(VenueNetwork::for_tier(AccountTier::Demo), VenueNetwork::Testnet);
        assert_eq!(VenueNetwork::for_tier(AccountTier::Live), VenueNetwork::Mainnet);
        let fields = CredentialFields::from_pairs([(PRIVATE_KEY, "0xabc123")]);
        let c = load(AccountTier::Paper, &holding(&fields)).expect("the account's key");
        assert_eq!(c.network, VenueNetwork::Testnet);
    }

    #[test]
    fn demo_loads_testnet_with_optional_master_address() {
        let fields = CredentialFields::from_pairs([
            (PRIVATE_KEY, "0xabc123"),
            (ACCOUNT_ADDRESS, "0xAbCdEf0123456789aBcDeF0123456789AbCdEf01"),
        ]);
        let c = load(AccountTier::Demo, &holding(&fields)).expect("loads");
        assert_eq!(c.network, VenueNetwork::Testnet);
        assert_eq!(phantom_source(c.network), "b");
        // address lowercased for signing
        assert_eq!(
            c.account_address.as_deref(),
            Some("0xabcdef0123456789abcdef0123456789abcdef01")
        );
        // secret never rendered
        assert!(!format!("{c:?}").contains("0xabc123"));
    }

    /// **The rows and the signing identity follow ONE network** (PLAN trap 2): on the release's seed
    /// each network's `/info`, `/exchange` and `/ws` are, byte for byte, the addresses dialled before
    /// they moved into rows, and the network that picked them is the one whose source byte and chain
    /// name sign. A demo (`Testnet`) lookup never reads a live row for its trade or user channels.
    #[test]
    fn the_rows_and_the_signature_follow_the_same_network() {
        use crate::transport::consts::{EXCHANGE_PATH, INFO_PATH};
        let seed = VenueUrls::seed(crate::transport::consts::VENUE);
        let rest =
            |n: VenueNetwork| base_url(n, &seed, UrlChannel::Trade, UrlProtocol::Rest).unwrap();
        for (net, host, source, chain) in [
            (VenueNetwork::Mainnet, "api.hyperliquid.xyz", "a", "Mainnet"),
            (VenueNetwork::Testnet, "api.hyperliquid-testnet.xyz", "b", "Testnet"),
        ] {
            assert_eq!(format!("{}{INFO_PATH}", rest(net)), format!("https://{host}/info"));
            assert_eq!(format!("{}{EXCHANGE_PATH}", rest(net)), format!("https://{host}/exchange"));
            assert_eq!(
                websocket_url(net, &seed, UrlChannel::User).unwrap(),
                format!("wss://{host}/ws")
            );
            assert_eq!(
                websocket_url(net, &seed, UrlChannel::Data).unwrap(),
                format!("wss://{host}/ws")
            );
            assert_eq!((phantom_source(net), hyperliquid_chain(net)), (source, chain));
        }
        // History reads mainnet for a testnet account too (no demo history row; data falls back).
        let history =
            base_url(VenueNetwork::Testnet, &seed, UrlChannel::History, UrlProtocol::Rest);
        assert_eq!(history, Ok("https://api.hyperliquid.xyz"));
    }

    #[test]
    fn key_without_address_means_key_is_the_account() {
        let fields = CredentialFields::from_pairs([(PRIVATE_KEY, "0xdeadbeef")]);
        let c = load(AccountTier::Live, &holding(&fields)).expect("loads");
        assert_eq!(c.network, VenueNetwork::Mainnet);
        assert!(c.account_address.is_none());
    }

    /// **The fields the loader reads are `vike_venues::credential_schema`'s hyperliquid row, word
    /// for word**: [`PRIVATE_KEY`] and [`ACCOUNT_ADDRESS`] are exactly the row's ACCOUNT fields, the
    /// key is the one required field, and an account holding exactly the required fields
    /// configures the loader at either network.
    #[test]
    fn the_fields_are_the_schemas() {
        use std::collections::BTreeSet;
        use vike_venues::credential_schema::{KeyFieldOwner, schema_for};
        let schema =
            schema_for(crate::transport::consts::VENUE).expect("hyperliquid is a schema row");
        let ours: BTreeSet<&str> = [PRIVATE_KEY, ACCOUNT_ADDRESS].into_iter().collect();
        let theirs: BTreeSet<&str> = schema.account_fields().map(|f| f.field).collect();
        assert_eq!(ours, theirs, "the loader's fields are the schema row's account fields");
        assert!(schema.venue_fields().next().is_none(), "hyperliquid holds no venue field");
        let required = schema.required(KeyFieldOwner::Account);
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
