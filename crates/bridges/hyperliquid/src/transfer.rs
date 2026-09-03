//! Hyperliquid `usdClassTransfer` — move USDC between the spot and perp balances (a USER-signed
//! EIP-712 action, distinct from the phantom-agent L1 signing in [`crate::signing`]).
//!
//! # What this is
//! `usdClassTransfer` rebalances USDC between the SAME account's spot and perp wallets
//! (`to_perp = true` moves spot → perp; `false` moves perp → spot). It is an INTERNAL transfer —
//! the funds never leave the account — so it is instant and free, and (unlike a withdrawal) it
//! cannot exfiltrate funds to any other address.
//!
//! # Signing scheme (ported from the official Python SDK `signing.py`/`exchange.py`)
//! A **user-signed** EIP-712 action, NOT the L1 phantom-agent scheme used for orders:
//! - domain: `("HyperliquidSignTransaction", "1", chainId 0x66eee = 421614, verifyingContract 0x0)`.
//! - primaryType: `HyperliquidTransaction:UsdClassTransfer(string hyperliquidChain,string amount,
//!   bool toPerp,uint64 nonce)` (the literal type name carries the `:` — an HL convention).
//! - action JSON: `{"type":"usdClassTransfer","amount","toPerp","nonce","signatureChainId":"0x66eee",
//!   "hyperliquidChain":"Mainnet"|"Testnet"}` (field order mirrors the SDK's dict-insertion order;
//!   unlike the L1 msgpack path it is NOT signature-load-bearing — the signature is over the EIP-712
//!   typed data built from the named fields, so JSON key order is cosmetic).
//! - `/exchange` envelope: `{action, nonce, signature}`, where the top-level `nonce` equals the
//!   action's `nonce`; the SDK's `vaultAddress`/`expiresAfter` are `None` for `usdClassTransfer`, so
//!   they are omitted here (omitted, never `null`).
//!
//! The digest + signing live in [`crate::signing::eip712::sign_usd_class_transfer`], reusing the same
//! keccak/secp256k1 EIP-712 primitives as the L1 path.
//!
//! # Agent (API) wallet CAN sign this — the credential-model verdict
//! This app holds the AGENT (API-wallet) private key + the master ACCOUNT ADDRESS only, never the
//! master key. The research verdict is that an agent wallet **can** sign `usdClassTransfer`, so this
//! is actionable in the app's credential model. Evidence:
//!   1. The Python SDK's `usd_class_transfer` signs with `self.wallet` **uniformly** — no
//!      master-only branch — and `self.wallet` is routinely an approved agent/API wallet.
//!   2. The Rust SDK's equivalent `class_transfer` (identical spot↔perp semantics) is signed with the
//!      **L1 phantom-agent** scheme (`sign_l1_action`) — the scheme agents sign — so HL's design
//!      intent plainly permits an agent to move the master's own funds between spot and perp.
//!   3. Security model: the agent-wallet restriction applies to funds LEAVING the account
//!      (`withdraw`/`usdSend`/`spotSend` are master-only, since a leaked API key must not be able to
//!      steal). `usdClassTransfer` keeps funds inside the account, so it is not in that class. The
//!      venue recovers the agent address from the signature and operates on the master account it is
//!      approved for (agents hold no funds of their own).
//!
//! # UNVERIFIED live
//! This is **SDK-spec-faithful but has NO golden vector and was NOT live-verified** (the linked
//! account is unified; a real testnet round-trip is owed before trusting it with funds). The tests
//! below prove (a) the exact wire shape/field-order against the SDK, and (b) that the produced
//! signature recovers the signer's own address (the EIP-712 digest + secp256k1 round-trip). They do
//! **not** prove the venue accepts it. Sub-account/vault transfers (the SDK's
//! `amount += " subaccount:{addr}"` form) are intentionally out of scope — this moves the signing
//! account's own USDC.

use serde::Serialize;
use serde_json::Value;

use vike_bridge_core::transport::VenueApiError;

use crate::config::Network;
use crate::consts::{USER_SIGNED_CHAIN_ID, VENUE};
use crate::signing::{eip712, Signature, Signer};
use crate::transport::HyperliquidTransport;

/// The `signatureChainId` action field — HL's fixed `"0x66eee"` (the SAME hex the Python SDK
/// hardcodes for BOTH mainnet and testnet). `int("0x66eee", 16)` == [`USER_SIGNED_CHAIN_ID`]
/// (`421614`, Arbitrum Sepolia), which is the numeric `chainId` the EIP-712 domain is built with; a
/// test below pins the two representations together.
const SIGNATURE_CHAIN_ID: &str = "0x66eee";

/// The signed `usdClassTransfer` action. Field DECLARATION order reproduces the Python SDK's action
/// dict (`{type, amount, toPerp, nonce}` then `signatureChainId`/`hyperliquidChain` appended), so
/// `serde_json::to_string` emits exactly the SDK's byte shape. (Only for the human-facing wire; the
/// signature is over the EIP-712 typed data, so this order is not itself signature-load-bearing.)
#[derive(Serialize, Clone, Debug)]
struct UsdClassTransferAction {
    #[serde(rename = "type")]
    action_type: &'static str,
    /// Decimal string (the transfer amount in USDC). Signed as an EIP-712 `string`.
    amount: String,
    #[serde(rename = "toPerp")]
    to_perp: bool,
    nonce: u64,
    #[serde(rename = "signatureChainId")]
    signature_chain_id: &'static str,
    #[serde(rename = "hyperliquidChain")]
    hyperliquid_chain: &'static str,
}

/// The `/exchange` POST body for a user-signed action: `{action, nonce, signature}`. `nonce` repeats
/// the action's nonce (the SDK posts both); `vaultAddress`/`expiresAfter` are absent for
/// `usdClassTransfer`.
#[derive(Serialize)]
struct TransferBody<'a> {
    action: &'a UsdClassTransferAction,
    nonce: u64,
    signature: Signature,
}

/// Build + sign the `usdClassTransfer` action for a fixed `nonce` (no network) — the unit-testable
/// core (the twin of `transport::exchange_body`). `network` selects the `hyperliquidChain` string;
/// the signature is produced by [`eip712::sign_usd_class_transfer`] over the same fields.
fn build_signed_action(
    signer: &Signer,
    amount: f64,
    to_perp: bool,
    network: Network,
    nonce: u64,
) -> (UsdClassTransferAction, Signature) {
    let action = UsdClassTransferAction {
        action_type: "usdClassTransfer",
        // `{}` renders a whole f64 WITHOUT a trailing ".0" (Python's `str(100.0)` == "100.0");
        // harmless — the venue parses the decimal and we sign the exact string we send.
        amount: format!("{amount}"),
        to_perp,
        nonce,
        signature_chain_id: SIGNATURE_CHAIN_ID,
        hyperliquid_chain: network.hyperliquid_chain(),
    };
    let signature = eip712::sign_usd_class_transfer(
        signer,
        action.hyperliquid_chain,
        &action.amount,
        action.to_perp,
        action.nonce,
        u128::from(USER_SIGNED_CHAIN_ID),
    );
    (action, signature)
}

/// Move `amount` USDC between the signer's spot and perp balances (`to_perp = true` ⇒ spot → perp).
///
/// Builds the user-signed `usdClassTransfer` action, signs it with `signer`'s key (EIP-712, user
/// domain — NOT the L1 order scheme), charges the venue IP-weight budget through `transport`'s shared
/// [`vike_bridge_core::ratelimit::RateGate`], and POSTs it **exactly once** to `network`'s
/// `/exchange` through `transport`'s own [`HyperliquidTransport::post`] — the same post+audit-T1
/// classification the order path rides (it can't go through `HyperliquidTransport::exchange`, which
/// bakes in L1 phantom-agent signing and only accepts the L1 `Action` enum; this file used to
/// hand-mirror `post` merely because it was private). Returns HL's raw `{status, response}` body on a
/// 2xx (the caller reads `status`), or a [`VenueApiError`] on failure. `network` must match
/// `transport`'s network (it selects both the host and the `hyperliquidChain` field).
///
/// UNVERIFIED live — see the module docs.
pub fn usd_class_transfer(
    transport: &HyperliquidTransport,
    signer: &Signer,
    amount: f64,
    to_perp: bool,
    network: Network,
) -> Result<Value, VenueApiError> {
    // One-shot operator action → a raw wall-clock ms nonce is sufficient (no same-ms burst to dedup,
    // unlike the exec order path's `crate::signing::NonceManager`).
    let nonce = vike_model::clock::now_ms_u64();
    let (action, signature) = build_signed_action(signer, amount, to_perp, network, nonce);
    let body = TransferBody { action: &action, nonce, signature };
    let payload = serde_json::to_vec(&body)
        .map_err(|e| VenueApiError { code: 0, msg: format!("bad transfer body: {e}") })?;
    // A single action is IP weight 1 (`transport::exchange_weight(0)`); ride the same per-IP budget
    // window as orders/reads so a manual transfer can't overrun it.
    transport.rate_gate().proceed_cost_logged(VENUE, "usdClassTransfer", 1);
    let (_, exchange_url, _) = network.urls();
    transport.post(exchange_url, &payload)
}

#[cfg(test)]
mod tests {
    //! SDK-faithful wire shape + a self-consistency (recover-the-signer) proof. No golden vector and
    //! NOT live-verified — these pin the EIP-712 digest/round-trip and the exact JSON, nothing more.
    use super::*;

    /// The official Rust SDK's test wallet (shared with `signing`/`transport` tests) — a valid
    /// secp256k1 key, so the `Signer` constructs and signing runs end-to-end.
    const KEY: &str = "e908f86dbb4d55ac876378565aafeabc187f6690f046459397b17d9b9a19688e";

    fn signer(network: Network) -> Signer {
        Signer::from_private_key(KEY, network).expect("SDK test key is valid")
    }

    #[test]
    fn signature_chain_id_string_matches_the_numeric_const() {
        // The action's hex string and the domain's numeric chainId are two views of ONE value.
        assert_eq!(SIGNATURE_CHAIN_ID, format!("0x{:x}", USER_SIGNED_CHAIN_ID));
        assert_eq!(USER_SIGNED_CHAIN_ID, 421_614);
    }

    #[test]
    fn action_json_is_the_exact_sdk_field_set_and_order() {
        let (action, _sig) = build_signed_action(
            &signer(Network::Testnet),
            25.5,
            true,
            Network::Testnet,
            1_700_000_000_123,
        );
        let json = serde_json::to_string(&action).expect("action serializes");
        assert_eq!(
            json,
            r#"{"type":"usdClassTransfer","amount":"25.5","toPerp":true,"nonce":1700000000123,"signatureChainId":"0x66eee","hyperliquidChain":"Testnet"}"#
        );
    }

    #[test]
    fn mainnet_selects_the_mainnet_hyperliquid_chain() {
        let (action, _sig) =
            build_signed_action(&signer(Network::Mainnet), 1.0, false, Network::Mainnet, 42);
        assert_eq!(action.hyperliquid_chain, "Mainnet");
        assert!(!action.to_perp);
    }

    #[test]
    fn amount_formats_as_a_plain_decimal_string() {
        // Whole numbers render without a trailing ".0"; fractional amounts keep their digits.
        let s = signer(Network::Mainnet);
        assert_eq!(build_signed_action(&s, 100.0, true, Network::Mainnet, 1).0.amount, "100");
        assert_eq!(build_signed_action(&s, 12.5, true, Network::Mainnet, 1).0.amount, "12.5");
    }

    #[test]
    fn exchange_body_carries_action_matching_nonce_and_signature() {
        let s = signer(Network::Mainnet);
        let nonce = 1_700_000_000_123u64;
        let (action, signature) = build_signed_action(&s, 100.5, false, Network::Mainnet, nonce);
        let body = TransferBody { action: &action, nonce, signature };
        let v: Value = serde_json::from_str(&serde_json::to_string(&body).unwrap()).unwrap();

        assert_eq!(v["action"]["type"], "usdClassTransfer");
        assert_eq!(v["action"]["amount"], "100.5");
        assert_eq!(v["action"]["toPerp"], false);
        assert_eq!(v["action"]["nonce"].as_u64(), Some(nonce));
        assert_eq!(v["action"]["signatureChainId"], "0x66eee");
        assert_eq!(v["action"]["hyperliquidChain"], "Mainnet");
        // top-level nonce repeats the action nonce.
        assert_eq!(v["nonce"].as_u64(), Some(nonce));
        // signature is the `{r,s,v}` object (r/s = 0x + 64 hex, v ∈ {27,28}).
        let sig = &v["signature"];
        let r = sig["r"].as_str().expect("r present");
        let s2 = sig["s"].as_str().expect("s present");
        assert!(r.starts_with("0x") && r.len() == 66, "malformed r: {r}");
        assert!(s2.starts_with("0x") && s2.len() == 66, "malformed s: {s2}");
        assert!(matches!(sig["v"].as_u64(), Some(27) | Some(28)), "v: {}", sig["v"]);
        // user-signed usdClassTransfer omits vaultAddress/expiresAfter entirely.
        assert!(v.get("vaultAddress").is_none(), "vaultAddress must be absent");
        assert!(v.get("expiresAfter").is_none(), "expiresAfter must be absent");
    }

    #[test]
    fn signature_is_deterministic_for_a_fixed_action() {
        // RFC-6979 deterministic ECDSA ⇒ the same inputs produce the same signature.
        let s = signer(Network::Mainnet);
        let a = build_signed_action(&s, 7.0, true, Network::Mainnet, 99).1;
        let b = build_signed_action(&s, 7.0, true, Network::Mainnet, 99).1;
        assert_eq!(a, b);
    }

    /// The proof that the EIP-712 digest + signing round-trips: recover the public key from the
    /// produced `{r,s,v}` over the recomputed digest and derive its Ethereum address — it MUST equal
    /// the signer's own address. (This validates the crypto end-to-end; it does NOT prove the venue
    /// accepts the action — there is no golden vector and it is not live-verified.)
    #[test]
    fn signature_recovers_the_signers_address() {
        use k256::ecdsa::{RecoveryId, Signature as EcdsaSig, VerifyingKey};

        let s = signer(Network::Mainnet);
        let (amount, to_perp, nonce) = (42.0_f64, true, 1_700_000_000_123u64);
        let chain = Network::Mainnet.hyperliquid_chain();
        let (_action, signature) =
            build_signed_action(&s, amount, to_perp, Network::Mainnet, nonce);

        // The exact digest the signer signed.
        let digest = eip712::usd_class_transfer_digest(
            chain,
            &format!("{amount}"),
            to_perp,
            nonce,
            u128::from(USER_SIGNED_CHAIN_ID),
        );

        // Reassemble r‖s, recover the verifying key, derive keccak256(pubkey[1..])[12..] → address.
        let r = hex::decode(signature.r.strip_prefix("0x").unwrap()).unwrap();
        let sbytes = hex::decode(signature.s.strip_prefix("0x").unwrap()).unwrap();
        let mut rs = [0u8; 64];
        rs[..32].copy_from_slice(&r);
        rs[32..].copy_from_slice(&sbytes);
        let ecdsa_sig = EcdsaSig::from_slice(&rs).expect("valid r,s");
        let recid = RecoveryId::from_byte(signature.v - 27).expect("v ∈ {27,28}");
        let vk = VerifyingKey::recover_from_prehash(&digest, &ecdsa_sig, recid)
            .expect("recovery succeeds for a signature we just produced");
        let point = vk.to_sec1_point(false); // 0x04 ‖ X ‖ Y
        let addr = format!("0x{}", hex::encode(&eip712::keccak256(&point.as_bytes()[1..])[12..]));

        assert_eq!(addr, s.address(), "the signature must recover the signer's own address");
    }
}
