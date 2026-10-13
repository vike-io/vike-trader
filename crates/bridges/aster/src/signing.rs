//! Aster v3 request auth — EIP-712 wallet signatures (NOT Binance HMAC). Every signed request
//! (orders, listenKey, account, recon) is authenticated by signing an EIP-712 `Message{ string msg }`
//! with the API-wallet (agent) private key, where `msg` is the urlencoded business params plus
//! `nonce` (µs) + `user` (master addr) + `signer` (agent addr). Domain
//! `{AsterSignTransaction, "1", chainId 1666, verifyingContract 0x0}`. Implements the shared
//! `vike_bridge_core::signer::Signer` seam so the ExecActor/VenueRest/transport stack is reused unchanged.
//!
//! Byte-exactness is the risk: a wrong field silently rejects an order. The msg param ordering
//! (insertion order per the official demo vs ASCII-sorted per the overview) is centralized HERE and
//! locked by the testnet smoke; flip `ORDER_SORTED` if the round-trip shows sorting is required.

use std::sync::atomic::{AtomicI64, Ordering};

use vike_bridge_core::eip712::{
    digest, domain_separator, enc_string, eth_address_from_private_key, hash_struct,
    sign_digest_hex,
};
use vike_bridge_core::signer::{PreparedRequest, Signer};
use vike_model::secret::Secret;
use vike_secrets::account_credentials::AccountCredentials;
use vike_secrets::credentials::Credentials;

const ASTER_DOMAIN_NAME: &str = "AsterSignTransaction";
const ASTER_DOMAIN_VERSION: &str = "1";
const ASTER_CHAIN_ID: u128 = 1666;
const ZERO_ADDR: &str = "0x0000000000000000000000000000000000000000";
/// See module doc — the one place param ordering is decided.
const ORDER_SORTED: bool = false;

/// EIP-712 v3 signer. Holds the master `user` address, the agent `signer` address (derived from the
/// key when not supplied), and the agent private key (secret — redacted in `Debug`).
pub struct AsterSigner {
    user: String,
    signer_addr: String,
    private_key: String,
    now_us: Box<dyn Fn() -> i64 + Send + Sync>,
    offset_us: AtomicI64,
}

impl AsterSigner {
    /// `creds.api_key` = master `user` address, `creds.api_secret` = agent private key,
    /// `creds.passphrase` = agent `signer` address (optional; derived from the key when absent).
    pub fn new(creds: &Credentials, now_us: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        let private_key = creds.api_secret.expose().clone();
        let signer_addr = creds
            .passphrase
            .as_ref()
            .map(|p| p.expose().clone())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| eth_address_from_private_key(&private_key).unwrap_or_default());
        AsterSigner {
            user: creds.api_key.expose().clone(),
            signer_addr,
            private_key,
            now_us: Box::new(now_us),
            offset_us: AtomicI64::new(0),
        }
    }

    /// Apply a server-time skew correction (µs).
    pub fn set_offset_us(&self, offset_us: i64) {
        self.offset_us.store(offset_us, Ordering::Relaxed);
    }

    /// The ordered auth params (`params + nonce + user + signer`), sorted iff `ORDER_SORTED`. The
    /// signed `msg` AND the final query are both built from this EXACT ordering, so the server
    /// reconstructs the identical string it verifies against (a mismatch silently rejects).
    fn ordered_params(&self, params: &[(&str, String)], nonce: i64) -> Vec<(String, String)> {
        let mut all: Vec<(String, String)> =
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        all.push(("nonce".to_string(), nonce.to_string()));
        all.push(("user".to_string(), self.user.clone()));
        all.push(("signer".to_string(), self.signer_addr.clone()));
        if ORDER_SORTED {
            all.sort_by(|a, b| a.0.cmp(&b.0));
        }
        all
    }
}

impl Signer for AsterSigner {
    fn prepare(&self, params: &[(&str, String)], _method: &str, _path: &str) -> PreparedRequest {
        let nonce = (self.now_us)() + self.offset_us.load(Ordering::Relaxed);
        let ordered = self.ordered_params(params, nonce);
        let mut refs: Vec<(&str, String)> =
            ordered.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        let msg = vike_bridge_core::urlencode(&refs);
        let dom =
            domain_separator(ASTER_DOMAIN_NAME, ASTER_DOMAIN_VERSION, ASTER_CHAIN_ID, ZERO_ADDR);
        let struct_hash = hash_struct("Message(string msg)", &[enc_string(&msg)]);
        let signature =
            sign_digest_hex(&digest(&dom, &struct_hash), &self.private_key).unwrap_or_default();
        // Final query = the SAME ordered params the msg was signed over, plus the signature.
        refs.push(("signature", signature));
        PreparedRequest { query: vike_bridge_core::urlencode(&refs), body: None, headers: vec![] }
    }
}

impl std::fmt::Debug for AsterSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tail = if self.user.len() >= 4 { &self.user[self.user.len() - 4..] } else { "" };
        write!(f, "AsterSigner(user=…{tail}, signer=set, key=***)")
    }
}

// The account FIELDS this venue reads — the words of `vike_model::credential_schema`'s aster row,
// held to it by `crates/bridges/aster/src/signing_tests.rs`'s `the_fields_are_the_schemas`.
/// The account field holding the MASTER address — the book, sent on the wire as `user=`. Required.
pub(crate) const USER: &str = "USER";
/// The account field holding the agent (API) wallet's private key, which signs. Required.
pub(crate) const PRIVATE_KEY: &str = "PRIVATE_KEY";
/// The account field holding the agent's signer ADDRESS — optional, derived from the key when absent.
pub(crate) const SIGNER: &str = "SIGNER";

/// Load Aster's agent-wallet credentials from the mounted ACCOUNT's fields (decision 0156 item 6: a
/// credential belongs to its account, and no name is composed here). Returns `None` (the live gate
/// → do not run) unless BOTH `USER` and `PRIVATE_KEY` are present (non-blank); `SIGNER` is
/// optional (derived from the key). Mapped onto the generic `Credentials` slots so
/// `AsterSigner::new` and the shared transport reuse.
///
/// The fields carry no tier: the account HAS one (`account.tier`), and the network the caller
/// signs on is that tier's (`crate::urls::urls_for`) — a `live` account's pair is a MAINNET agent
/// wallet, a `demo` account's a testnet one. There is no fallback from one account's set to
/// another's, so a labelled account holding no pair of its own reads as ABSENT. ⚠ Aster is MAINNET
/// in practice (`crates/bridges/aster/CLAUDE.md`), so that is a real-money guarantee rather than a
/// tidy one.
pub fn load_aster_credentials(creds: &AccountCredentials<'_>) -> Option<Credentials> {
    let user = creds.get(USER)?;
    let private_key = creds.get(PRIVATE_KEY)?;
    Some(Credentials {
        api_key: Secret::new(user.to_string()),
        api_secret: Secret::new(private_key.to_string()),
        passphrase: creds.secret(SIGNER),
    })
}

#[path = "signing_tests.rs"]
#[cfg(test)]
mod signing_tests;
