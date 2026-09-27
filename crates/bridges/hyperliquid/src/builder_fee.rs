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
//!   "maxFeeRate","builder","nonce"}`. ⚠ This ORDER is ours, not the SDK's, and this line claimed
//!   otherwise until it was checked against the source on 2026-09-14: `exchange.py`'s
//!   `approve_builder_fee` inserts `{maxFeeRate, builder, nonce, type}` and `signing.py`'s
//!   `sign_user_signed_action` then appends `signatureChainId`/`hyperliquidChain`, so the SDK's
//!   own dict order is `maxFeeRate, builder, nonce, type, signatureChainId, hyperliquidChain`.
//!   Harmless, and the reason it is harmless is the part that matters: unlike the L1 msgpack path,
//!   JSON key order here is NOT signature-load-bearing — the signature is over the EIP-712 typed
//!   data built from the NAMED fields. The field NAMES are the protocol constants, and they do
//!   match the SDK verbatim. (`usdClassTransfer`'s order genuinely does mirror its SDK dict.)
//! - `/exchange` envelope: `{action, nonce, signature}`; `vaultAddress`/`expiresAfter` are omitted
//!   (never `null`), matching every other user-signed action in this crate.
//!
//! The digest + signing live in [`crate::signing::eip712::sign_approve_builder_fee`], reusing the
//! same keccak/secp256k1 EIP-712 primitives as the L1 and `usdClassTransfer` paths.
//!
//! # ⚠ MASTER WALLET ONLY — measured, and the reason is not the one this section used to give
//! HL requires the grant to come from the **master account's own key**: an agent is approved FOR
//! builder-coded orders, it cannot approve a builder's fee ON the account's behalf. The caller MUST
//! pass a [`crate::signing::Signer`] built from the master private key.
//!
//! ⚠ **This opened "Unlike `usdClassTransfer` (which an agent/API wallet CAN sign)" until
//! 2026-09-14, and that contrast does not exist.**
//! `crates/bridges/hyperliquid/tests/hyperliquid_permissioning_smoke.rs` put both actions to the
//! TESTNET `/exchange` signed by a key the venue itself reports (`/info userRole`) as an APPROVED
//! AGENT of a funded master, and got the SAME answer to each:
//!
//! ```text
//! {"status":"err","response":"Must deposit before performing actions. User: 0x<THE AGENT>"}
//! ```
//!
//! So the rule this section states is CORRECT for an agent — but it is not special to this action,
//! and `crate::transfer`'s opposing claim is the one that was wrong (that module's doc now records
//! the falsification). The venue draws its line between USER-SIGNED actions and L1 ones, not
//! between internal and external movement: a user-signed action resolves to the SIGNER's own
//! account, so an agent signing one is asking on its own behalf, not the master's.
//!
//! The old parenthetical — "signs a well-formed but venue-rejected request (or worse, silently
//! approves nothing)" — is now measured, and it is the first half: the request is REJECTED, loudly,
//! and `/info maxBuilderFee(master, builder)` is unchanged afterwards (asserted by that smoke, so a
//! grant recorded against the master despite the error would go red). Nothing is silent.
//!
//! ⚠ What is NOT proven is the POSITIVE half — that the MASTER key succeeds. The credential store
//! this was measured against holds an agent key by design and no master key, so every refusal here
//! is of a non-master signer. "Master-only" is established as "not-an-agent".
//!
//! This is why the helper bin (`bin/hyperliquid_builder_fee_approve.rs`, the `builder-fee-approve`
//! feature — mirrors Nautilus's `hyperliquid-builder-fee-approve`) is a separate,
//! deliberately-manual, opt-in tool, never invoked from the live exec/mount path. Its refusal when
//! `HYPERLIQUID_{tier}_ACCOUNT_ADDRESS` is set — i.e. when the loaded key is an agent — is now
//! vindicated by measurement rather than by inference.
//!
//! # LIVE-VERIFIED on testnet — the SIGNING is proven, and the MASTER-ONLY rule is proven NEGATIVELY
//! ⚠ This heading read **"the MASTER-ONLY rule is not"** until the permissioning smoke was written;
//! before that the whole section read **"UNVERIFIED live … a real testnet round-trip is owed before
//! trusting it with a live grant"**. The signing round-trip is the one
//! `crate::transfer` records: `crates/bridges/hyperliquid/tests/hyperliquid_user_signed_smoke.rs`
//! posts THIS module's own payload — through [`approve_builder_fee`] — to the TESTNET `/exchange`
//! and asserts the address Hyperliquid names back is the one the signing key derives. Because a
//! user-signed action carries no sender field, that answer is an ECDSA recovery over the VENUE's
//! own EIP-712 digest, so a match means its digest is byte-identical to ours — certifying the
//! `HyperliquidTransaction:ApproveBuilderFee` type string character for character, every field name
//! (`maxFeeRate` and `builder` included) and type, the field ORDER inside the type string, the
//! domain and the chain id, in one assertion. MEASURED 2026-09-14: it matched.
//!
//! ⚠ **The MASTER-WALLET-ONLY rule above is NOT what that proves**, and the distinction is the
//! whole reason this paragraph is separate from the one above it. That smoke signs with a throwaway
//! key the venue has never seen, which is answered *before* any master/agent authorization check
//! could matter — so it certifies that the grant is SPELLED correctly, never that a given wallet is
//! allowed to make it.
//!
//! The rule's NEGATIVE half is what
//! `crates/bridges/hyperliquid/tests/hyperliquid_permissioning_smoke.rs` then measured, with a
//! signer the venue KNOWS: an approved AGENT is refused, the venue names the AGENT as the principal
//! rather than the master, `maxBuilderFee` is unchanged afterwards, and a freshly minted stranger
//! draws the identical verdict — so agent approval is not consulted here at all. The POSITIVE half
//! (that the master key succeeds) is still unproven and, with an agent-keyed credential store, not
//! reachable; see that file's *What this file does NOT prove*. No SDK publishes a golden VECTOR for
//! this action either — still true, and the reason the guard is several files rather than one.
//!
//! ⚠ **Since 2026-09-14 the venue's answer no longer evaporates with the run.** The smoke signs
//! with a FIXED, funds-less throwaway key instead of an ephemeral one, so the run Hyperliquid
//! certified yields a `(key, nonce, action) -> signature` triple that can be written down:
//! `fixtures/hyperliquid_signed/venue_certified_user_signed.json` holds it beside the date, the
//! address the venue named and its verbatim reply, and
//! `crates/bridges/hyperliquid/tests/hyperliquid_venue_certified_vectors.rs` re-signs it OFFLINE on
//! every CI lane that builds this crate. Each of these files answers a DIFFERENT question and none
//! substitutes for another (no count here — the set has already grown once):
//! `signed_payload_fixtures.rs` answers *did the bytes change*, the user-signed smoke answers *were
//! they right when it last ran* (network, `#[ignore]`d, so no CI lane runs it), the certified
//! vectors carry that second answer into CI, and `hyperliquid_permissioning_smoke.rs` answers *who
//! may send them*. ⚠ A certified value may never be refreshed from this build's own output — only
//! another testnet run can move one, and the fixture says so in its own `_regeneration` field.
//!
//! The tests below are unchanged and prove what they always did: (a) the exact wire
//! shape/field-order against the SDK, and (b) that the produced signature recovers the signer's own
//! address locally.

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

/// The signed `approveBuilderFee` action. The field NAMES are the SDK's verbatim (they are what
/// the venue rebuilds the EIP-712 message from); the field DECLARATION order is NOT the SDK's dict
/// order and does not need to be — see this module's doc for the measured difference and why it is
/// cosmetic here but not on the L1 msgpack path.
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
/// Build + sign the `approveBuilderFee` `/exchange` body for a FIXED `nonce` and return the EXACT
/// bytes [`approve_builder_fee`] posts — no network, no clock, no rate gate. The twin of
/// [`crate::transfer::usd_class_transfer_payload`], and it exists for the same reason: `maxFeeRate`
/// and `builder` are protocol constants the venue re-derives the EIP-712 message from, and a guard
/// for them living in this file's own `mod tests` cannot survive the edit it exists to catch.
/// `tests/signed_payload_fixtures.rs` holds the frozen bytes, under `fixtures/`.
pub fn approve_builder_fee_payload(
    signer: &Signer,
    builder: &str,
    max_fee_rate: &str,
    network: Network,
    nonce: u64,
) -> Result<Vec<u8>, VenueApiError> {
    let (action, signature) = build_signed_action(signer, builder, max_fee_rate, network, nonce);
    let body = ApproveBuilderFeeBody { action: &action, nonce, signature };
    // From the STRUCT, never via `serde_json::Value` — see the transfer twin's note on key order.
    serde_json::to_vec(&body)
        .map_err(|e| VenueApiError { code: 0, msg: format!("bad approve_builder_fee body: {e}") })
}

/// The SIGNING is testnet-verified against the venue, and so is the MASTER-ONLY rule's NEGATIVE
/// half: an approved AGENT is refused and the grant does not appear. That the master SUCCEEDS is
/// still unproven. See the module doc.
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
    let payload = approve_builder_fee_payload(signer, builder, max_fee_rate, network, nonce)?;
    // A single action is IP weight 1 (`transport::exchange_weight(0)`); ride the same per-IP
    // budget window as orders/reads.
    transport.rate_gate().proceed_cost_logged(VENUE, "approveBuilderFee", 1);
    let (_, exchange_url, _) = network.urls();
    transport.post(exchange_url, &payload)
}

#[cfg(test)]
mod tests {
    //! SDK-faithful wire shape + a self-consistency (recover-the-signer) proof — the twin of
    //! `transfer::tests`. No golden vector. The VENUE's own verdict on the same payload is a
    //! separate, network-gated file (see the module doc's live-verified section); nothing here
    //! reaches it, and nothing here should be relaxed because it exists.
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
    /// equal the signer's own address. This validates the crypto end-to-end LOCALLY; it does not
    /// by itself prove the VENUE computes the same digest — that is the network-gated smoke named
    /// in the module doc.
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
