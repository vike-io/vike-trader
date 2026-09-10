//! Aster v3 request auth — EIP-712 wallet signatures (NOT Binance HMAC). Every signed request
//! (orders, listenKey, account, recon) is authenticated by signing an EIP-712 `Message{ string msg }`
//! with the API-wallet (agent) private key, where `msg` is the urlencoded business params plus
//! `nonce` (µs) + `user` (master addr) + `signer` (agent addr). Domain
//! `{AsterSignTransaction, "1", chainId 1666, verifyingContract 0x0}`. Implements the shared
//! `vike_bridge_core::Signer` seam so the ExecActor/VenueRest/transport stack is reused unchanged.
//!
//! Byte-exactness is the risk: a wrong field silently rejects an order. The msg param ordering
//! (insertion order per the official demo vs ASCII-sorted per the overview) is centralized HERE and
//! locked by the testnet smoke; flip `ORDER_SORTED` if the round-trip shows sorting is required.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_bridge_core::credentials::account_var;
use vike_bridge_core::eip712::{
    digest, domain_separator, enc_string, eth_address_from_private_key, hash_struct,
    sign_digest_hex,
};
use vike_bridge_core::{Credentials, Environment, PreparedRequest, Signer};
use vike_model::account_keys::AccountLabel;

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
        let private_key = creds.api_secret.clone();
        let signer_addr = creds
            .passphrase
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| eth_address_from_private_key(&private_key).unwrap_or_default());
        AsterSigner {
            user: creds.api_key.clone(),
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

/// Load Aster's agent-wallet credentials from a var map (process env or parsed `.env`). Returns
/// `None` (the live gate → stay paper) unless BOTH `USER` and `PRIVATE_KEY` are present. `Live` →
/// `ASTER_LIVE_*`; everything else → `ASTER_TESTNET_*`. `SIGNER` is optional (derived from the key).
/// Mapped onto the generic `Credentials` slots so `AsterSigner::new` and the shared transport reuse.
///
/// ⚠ This is [`load_aster_credentials_for_account`] at
/// [`AccountLabel::Default`](vike_model::account_keys::AccountLabel::Default), reached through it —
/// so a single-account box reads the SAME `String` keys it always did, by construction rather than
/// by care (`vike_model::account_keys::account_key` returns its input unchanged for that account).
pub fn load_aster_credentials(
    env: Environment,
    vars: &HashMap<String, String>,
) -> Option<Credentials> {
    load_aster_credentials_for_account(env, &AccountLabel::Default, vars)
}

/// [`load_aster_credentials`] for ONE NAMED ACCOUNT — `ASTER_{TIER}_{SUFFIX}__{LABEL}`.
///
/// Everything [`load_aster_credentials`] documents holds word for word; the ONLY difference is the
/// NAMES read, and they are composed by `vike_bridge_core::credentials::account_var`, which appends
/// the label after the WHOLE of today's key. `ASTER_LIVE_PRIVATE_KEY` — a two-word bespoke suffix —
/// therefore needs no entry in any table.
///
/// ⚠ **No fallback to the unlabelled key**: a labelled account with no `USER`/`PRIVATE_KEY` of its
/// own reads as ABSENT (the live gate, unchanged), because the alternative is signing account
/// `ALT`'s orders with the default account's key. ⚠ Aster is MAINNET in practice
/// (`crates/bridges/aster/CLAUDE.md`), so that is a real-money guarantee rather than a tidy one.
pub fn load_aster_credentials_for_account(
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> Option<Credentials> {
    let tier = match env {
        Environment::Live => "LIVE",
        _ => "TESTNET",
    };
    let get = |k: &str| account_var(vars, &format!("ASTER_{tier}_{k}"), label).map(str::to_string);
    let user = get("USER")?;
    let private_key = get("PRIVATE_KEY")?;
    Some(Credentials { api_key: user, api_secret: private_key, passphrase: get("SIGNER") })
}

#[cfg(test)]
mod tests {
    use super::*;

    // The canonical EIP-712 "Cow" key (keccak256("cow")) and its known EOA — reused from the shared
    // primitive's spec vectors so we know the derived signer address is correct.
    const COW_KEY: &str = "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4";
    const COW_ADDR: &str = "0xcd2a3d9f938e13cd947ec05abc7fe734df8dd826";

    fn signer_with_fixed_nonce() -> AsterSigner {
        let creds = Credentials {
            api_key: "0xMASTER".to_string(),
            api_secret: COW_KEY.to_string(),
            passphrase: None, // force signer derivation from the key
        };
        AsterSigner::new(&creds, || 1_700_000_000_000_000)
    }

    #[test]
    fn signer_addr_derived_from_key_when_absent() {
        let s = signer_with_fixed_nonce();
        assert_eq!(s.signer_addr, COW_ADDR);
    }

    #[test]
    fn prepare_is_deterministic_and_carries_auth_fields() {
        let s = signer_with_fixed_nonce();
        let params = [("symbol", "BTCUSDT".to_string()), ("side", "BUY".to_string())];
        let a = s.prepare(&params, "POST", "/fapi/v3/order");
        let b = s.prepare(&params, "POST", "/fapi/v3/order");
        assert_eq!(a.query, b.query, "fixed nonce ⇒ deterministic");
        assert!(a.query.contains("user=0xMASTER"));
        assert!(a.query.contains(&format!("signer={COW_ADDR}")));
        assert!(a.query.contains("nonce=1700000000000000"));
        assert!(a.query.contains("signature=0x"));
        assert!(a.headers.is_empty(), "v3 auth is in the query, not headers");
    }

    #[test]
    fn nonce_binds_the_signature() {
        let creds = Credentials {
            api_key: "0xMASTER".to_string(),
            api_secret: COW_KEY.to_string(),
            passphrase: Some(COW_ADDR.to_string()),
        };
        let s1 = AsterSigner::new(&creds, || 1_700_000_000_000_000);
        let s2 = AsterSigner::new(&creds, || 1_700_000_000_000_001);
        let p = [("symbol", "BTCUSDT".to_string())];
        assert_ne!(s1.prepare(&p, "POST", "/x").query, s2.prepare(&p, "POST", "/x").query);
    }

    #[test]
    fn loader_gate_and_tier() {
        let mut vars = HashMap::new();
        assert!(load_aster_credentials(Environment::Demo, &vars).is_none()); // absent ⇒ gate
        vars.insert("ASTER_TESTNET_USER".to_string(), "0xUser".to_string());
        vars.insert("ASTER_TESTNET_PRIVATE_KEY".to_string(), COW_KEY.to_string());
        let c = load_aster_credentials(Environment::Demo, &vars).expect("testnet creds");
        assert_eq!(c.api_key, "0xUser");
        assert_eq!(c.api_secret, COW_KEY);
        // Live tier reads LIVE_*, which is absent here
        assert!(load_aster_credentials(Environment::Live, &vars).is_none());
    }

    #[test]
    fn debug_redacts_key() {
        let dbg = format!("{:?}", signer_with_fixed_nonce());
        assert!(!dbg.contains(COW_KEY), "private key must never appear in Debug");
        assert!(dbg.contains("key=***"));
    }
}
