//! Hyperliquid `approveBuilderFee` — the one-time on-chain grant a builder address needs before HL
//! accepts a **nonzero** [`crate::signing::action::HlBuilderFee::fee_tenths_bp`] on an order this
//! wallet places carrying that builder's code (a USER-signed EIP-712 action, distinct from the
//! phantom-agent L1 signing in [`crate::signing`] that orders/cancels/modify use).
//!
//! # What this is
//! Grants `builder` permission to charge up to `max_fee_rate` (a percentage string, e.g. `"0.01%"`)
//! on every subsequent order this account places with that builder's code attached. It is a
//! **standing grant**, not a per-order action: run it once per `(account, builder, rate)` tuple,
//! not on every submit. **Attribution-only mounts (fee = 0, this repo's default — resolved at
//! mount time in `vike-mount`'s `hyperliquid_live_client` and threaded through
//! [`crate::exec::HyperliquidExecutionClient::spawn`]) never need this at all**: a
//! [`crate::signing::action::HlBuilderFee`] with `fee_tenths_bp: 0` requires no prior approval (HL
//! only gates a *nonzero* fee).
//!
//! # Signing scheme (ported from the official Python SDK `signing.py`/`exchange.py`)
//! A **user-signed** EIP-712 action — the same domain/pattern as [`crate::transfer`]'s
//! `usdClassTransfer`, a different `primaryType`:
//! - domain: `("HyperliquidSignTransaction", "1", chainId 0x66eee = 421614, verifyingContract 0x0)`.
//! - primaryType: `HyperliquidTransaction:ApproveBuilderFee(string hyperliquidChain,string
//!   maxFeeRate,address builder,uint64 nonce)`.
//! - action JSON: `{"type":"approveBuilderFee","hyperliquidChain","signatureChainId":"0x66eee",
//!   "maxFeeRate","builder","nonce"}` (field order mirrors the SDK's dict-insertion order; like
//!   `usdClassTransfer`, this is NOT signature-load-bearing — the signature is over the EIP-712
//!   typed data built from the named fields, so JSON key order is cosmetic).
//! - `/exchange` envelope: `{action, nonce, signature}`; `vaultAddress`/`expiresAfter` are omitted
//!   (never `null`), matching every other user-signed action in this crate.
//!
//! The digest + signing live in [`crate::signing::eip712::sign_approve_builder_fee`], reusing the
//! same keccak/secp256k1 EIP-712 primitives as the L1 and `usdClassTransfer` paths.
//!
//! # ⚠ MASTER WALLET ONLY
//! Unlike `usdClassTransfer` (which an agent/API wallet CAN sign — see that module's doc), HL
//! requires the grant itself to come from the **master account's own key**: an agent is approved
//! FOR builder-coded orders, it cannot approve a builder's fee ON the account's behalf. The caller
//! MUST pass a [`crate::signing::Signer`] built from the master private key — passing an agent
//! wallet's key here signs a well-formed but venue-rejected request (or worse, silently approves
//! nothing, since the recovered signer address won't match the account). This is why the helper
//! bin (`bin/hyperliquid_builder_fee_approve.rs`, the `builder-fee-approve` feature — mirrors
//! Nautilus's `hyperliquid-builder-fee-approve`) is a separate, deliberately-manual, opt-in tool —
//! never invoked from the live exec/mount path.
//!
//! # UNVERIFIED live
//! Same caveat as `usdClassTransfer`: this is **SDK-spec-faithful but has NO golden vector and was
//! NOT live-verified**. The tests below prove (a) the exact wire shape/field-order against the SDK,
//! and (b) that the produced signature recovers the signer's own address. They do **not** prove the
//! venue accepts it — a real testnet round-trip is owed before trusting it with a live grant.

use serde::Serialize;
use serde_json::Value;

use vike_bridge_core::transport::VenueApiError;

use crate::config::Network;
use crate::consts::{USER_SIGNED_CHAIN_ID, VENUE};
use crate::signing::{Signature, Signer, eip712};
use crate::transport::HyperliquidTransport;

/// The `signatureChainId` action field — HL's fixed `"0x66eee"`, shared with `usdClassTransfer`
/// (see that module's const of the same name/value; kept as its own private const here rather than
/// `pub(crate)`-shared, matching the existing per-module duplication in `transfer.rs`).
const SIGNATURE_CHAIN_ID: &str = "0x66eee";

/// The signed `approveBuilderFee` action. Field DECLARATION order reproduces the Python SDK's
/// action dict, so `serde_json::to_string` emits exactly the SDK's byte shape (cosmetic only — the
/// signature is over the EIP-712 typed data, not this JSON order).
#[derive(Serialize, Clone, Debug)]
struct ApproveBuilderFeeAction {
    #[serde(rename = "type")]
    action_type: &'static str,
    #[serde(rename = "hyperliquidChain")]
    hyperliquid_chain: &'static str,
    #[serde(rename = "signatureChainId")]
    signature_chain_id: &'static str,
    /// A percentage string, e.g. `"0.01%"` — the SDK's `maxFeeRate` shape (NOT the wire order
    /// action's `fee_tenths_bp` integer; HL uses different encodings for the two calls).
    #[serde(rename = "maxFeeRate")]
    max_fee_rate: String,
    /// The builder address being approved (`0x…`, lowercased for signing consistency with the
    /// rest of this crate).
    builder: String,
    nonce: u64,
}

/// The `/exchange` POST body for a user-signed action: `{action, nonce, signature}` — identical
/// shape to `transfer::TransferBody`.
#[derive(Serialize)]
struct ApproveBuilderFeeBody<'a> {
    action: &'a ApproveBuilderFeeAction,
    nonce: u64,
    signature: Signature,
}

/// Build + sign the `approveBuilderFee` action for a fixed `nonce` (no network) — the
/// unit-testable core, the twin of `transfer::build_signed_action`.
fn build_signed_action(
    signer: &Signer,
    builder: &str,
    max_fee_rate: &str,
    network: Network,
    nonce: u64,
) -> (ApproveBuilderFeeAction, Signature) {
    let action = ApproveBuilderFeeAction {
        action_type: "approveBuilderFee",
        hyperliquid_chain: network.hyperliquid_chain(),
        signature_chain_id: SIGNATURE_CHAIN_ID,
        max_fee_rate: max_fee_rate.to_string(),
        builder: builder.trim().to_ascii_lowercase(),
        nonce,
    };
    let signature = eip712::sign_approve_builder_fee(
        signer,
        action.hyperliquid_chain,
        &action.max_fee_rate,
        &action.builder,
        action.nonce,
        u128::from(USER_SIGNED_CHAIN_ID),
    );
    (action, signature)
}

/// Grant `builder` permission to charge up to `max_fee_rate` (a percentage string, e.g. `"0.01%"`)
/// on this account's future builder-coded orders. **`signer` MUST be the MASTER wallet's key** —
/// see this module's doc. One-time per `(account, builder, rate)`; NOT needed at all for an
/// attribution-only (fee = 0) mount.
///
/// Builds the user-signed `approveBuilderFee` action, signs it (EIP-712, user domain — NOT the L1
/// order scheme), charges the venue IP-weight budget through `transport`'s shared
/// [`vike_bridge_core::ratelimit::RateGate`], and POSTs it **exactly once** to `network`'s
/// `/exchange` through `transport.post` (the same non-order user-signed path `transfer::
/// usd_class_transfer` uses). Returns HL's raw `{status, response}` body on a 2xx, or a
/// [`VenueApiError`] on failure. `network` must match `transport`'s network.
///
/// UNVERIFIED live — see the module doc.
pub fn approve_builder_fee(
    transport: &HyperliquidTransport,
    signer: &Signer,
    builder: &str,
    max_fee_rate: &str,
    network: Network,
) -> Result<Value, VenueApiError> {
    // One-shot operator action → a raw wall-clock ms nonce is sufficient (no same-ms burst to
    // dedup, unlike the exec order path's `crate::signing::NonceManager`) — mirrors
    // `transfer::usd_class_transfer`'s nonce choice.
    let nonce = vike_model::clock::now_ms_u64();
    let (action, signature) = build_signed_action(signer, builder, max_fee_rate, network, nonce);
    let body = ApproveBuilderFeeBody { action: &action, nonce, signature };
    let payload = serde_json::to_vec(&body).map_err(|e| VenueApiError {
        code: 0,
        msg: format!("bad approve_builder_fee body: {e}"),
    })?;
    // A single action is IP weight 1 (`transport::exchange_weight(0)`); ride the same per-IP
    // budget window as orders/reads.
    transport.rate_gate().proceed_cost_logged(VENUE, "approveBuilderFee", 1);
    let (_, exchange_url, _) = network.urls();
    transport.post(exchange_url, &payload)
}

#[cfg(test)]
mod tests {
    //! SDK-faithful wire shape + a self-consistency (recover-the-signer) proof — the twin of
    //! `transfer::tests`. No golden vector and NOT live-verified.
    use super::*;

    /// The official Rust SDK's test wallet (shared with `signing`/`transfer` tests).
    const KEY: &str = "e908f86dbb4d55ac876378565aafeabc187f6690f046459397b17d9b9a19688e";

    fn signer(network: Network) -> Signer {
        Signer::from_private_key(KEY, network).expect("SDK test key is valid")
    }

    #[test]
    fn action_json_is_the_exact_sdk_field_set_and_order() {
        let (action, _sig) = build_signed_action(
            &signer(Network::Testnet),
            "0x0c8de5f0362f6e4e9f0a4e3c1e1d2f3a4b5c6d7e",
            "0.01%",
            Network::Testnet,
            1_700_000_000_123,
        );
        let json = serde_json::to_string(&action).expect("action serializes");
        assert_eq!(
            json,
            r#"{"type":"approveBuilderFee","hyperliquidChain":"Testnet","signatureChainId":"0x66eee","maxFeeRate":"0.01%","builder":"0x0c8de5f0362f6e4e9f0a4e3c1e1d2f3a4b5c6d7e","nonce":1700000000123}"#
        );
    }

    #[test]
    fn builder_address_is_lowercased() {
        let (action, _sig) = build_signed_action(
            &signer(Network::Mainnet),
            "0x0C8DE5F0362F6E4E9F0A4E3C1E1D2F3A4B5C6D7E",
            "0%",
            Network::Mainnet,
            1,
        );
        assert_eq!(action.builder, "0x0c8de5f0362f6e4e9f0a4e3c1e1d2f3a4b5c6d7e");
    }

    #[test]
    fn mainnet_selects_the_mainnet_hyperliquid_chain() {
        let (action, _sig) = build_signed_action(
            &signer(Network::Mainnet),
            "0x0c8de5f0362f6e4e9f0a4e3c1e1d2f3a4b5c6d7e",
            "0.05%",
            Network::Mainnet,
            42,
        );
        assert_eq!(action.hyperliquid_chain, "Mainnet");
    }

    #[test]
    fn exchange_body_carries_action_matching_nonce_and_signature() {
        let s = signer(Network::Mainnet);
        let nonce = 1_700_000_000_123u64;
        let (action, signature) = build_signed_action(
            &s,
            "0x0c8de5f0362f6e4e9f0a4e3c1e1d2f3a4b5c6d7e",
            "0.02%",
            Network::Mainnet,
            nonce,
        );
        let body = ApproveBuilderFeeBody { action: &action, nonce, signature };
        let v: Value = serde_json::from_str(&serde_json::to_string(&body).unwrap()).unwrap();

        assert_eq!(v["action"]["type"], "approveBuilderFee");
        assert_eq!(v["action"]["maxFeeRate"], "0.02%");
        assert_eq!(v["action"]["builder"], "0x0c8de5f0362f6e4e9f0a4e3c1e1d2f3a4b5c6d7e");
        assert_eq!(v["action"]["nonce"].as_u64(), Some(nonce));
        assert_eq!(v["action"]["signatureChainId"], "0x66eee");
        assert_eq!(v["action"]["hyperliquidChain"], "Mainnet");
        assert_eq!(v["nonce"].as_u64(), Some(nonce));
        let sig = &v["signature"];
        let r = sig["r"].as_str().expect("r present");
        let s2 = sig["s"].as_str().expect("s present");
        assert!(r.starts_with("0x") && r.len() == 66, "malformed r: {r}");
        assert!(s2.starts_with("0x") && s2.len() == 66, "malformed s: {s2}");
        assert!(matches!(sig["v"].as_u64(), Some(27) | Some(28)), "v: {}", sig["v"]);
        assert!(v.get("vaultAddress").is_none(), "vaultAddress must be absent");
        assert!(v.get("expiresAfter").is_none(), "expiresAfter must be absent");
    }

    #[test]
    fn signature_is_deterministic_for_a_fixed_action() {
        let s = signer(Network::Mainnet);
        let a = build_signed_action(
            &s,
            "0x0c8de5f0362f6e4e9f0a4e3c1e1d2f3a4b5c6d7e",
            "0.01%",
            Network::Mainnet,
            99,
        )
        .1;
        let b = build_signed_action(
            &s,
            "0x0c8de5f0362f6e4e9f0a4e3c1e1d2f3a4b5c6d7e",
            "0.01%",
            Network::Mainnet,
            99,
        )
        .1;
        assert_eq!(a, b);
    }

    /// The proof that the EIP-712 digest + signing round-trips: recover the public key from the
    /// produced `{r,s,v}` over the recomputed digest and derive its Ethereum address — it MUST
    /// equal the signer's own address. This validates the crypto end-to-end; it does NOT prove the
    /// venue accepts the action.
    #[test]
    fn signature_recovers_the_signers_address() {
        use k256::ecdsa::{RecoveryId, Signature as EcdsaSig, VerifyingKey};

        let s = signer(Network::Mainnet);
        let (builder, max_fee_rate, nonce) =
            ("0x0c8de5f0362f6e4e9f0a4e3c1e1d2f3a4b5c6d7e", "0.01%", 1_700_000_000_123u64);
        let chain = Network::Mainnet.hyperliquid_chain();
        let (_action, signature) =
            build_signed_action(&s, builder, max_fee_rate, Network::Mainnet, nonce);

        let digest = eip712::approve_builder_fee_digest(
            chain,
            max_fee_rate,
            builder,
            nonce,
            u128::from(USER_SIGNED_CHAIN_ID),
        );

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
