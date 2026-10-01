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
//! # ⚠ An agent (API) wallet CANNOT sign this — the credential-model verdict, FALSIFIED
//! This section read **"Agent (API) wallet CAN sign this"** until 2026-09-14 and asserted that
//! `usdClassTransfer` was therefore actionable in this app's credential model, which holds the
//! AGENT (API-wallet) private key + the master ACCOUNT ADDRESS and never the master key. **The
//! venue says otherwise.** `crates/bridges/hyperliquid/tests/hyperliquid_permissioning_smoke.rs`
//! posts THIS module's own payload — through [`usd_class_transfer`] — to the TESTNET `/exchange`,
//! signed by a key the venue itself reports (`/info userRole`) as an APPROVED AGENT of a funded
//! master. MEASURED 2026-09-14:
//!
//! ```text
//! {"status":"err","response":"Must deposit before performing actions. User: 0x<THE AGENT>"}
//! ```
//!
//! ⚠ **The deposit wording is not the finding; the ADDRESS is.** A user-signed action carries no
//! sender field, so the address the venue names back is the principal it resolved the signature to
//! — and it named the AGENT, never the master. The venue applies a user-signed action to the
//! recovered address's OWN account. An agent wallet holds nothing by construction, so that is not a
//! state it can be funded out of. The smoke also re-runs each action under a freshly minted key the
//! venue has never seen and asserts the two verdicts are equal once the named address is redacted:
//! an approved agent and a total stranger are indistinguishable here, so **agent approval is not
//! consulted for this action at all**.
//!
//! Nothing in this crate calls [`usd_class_transfer`] (`git grep usd_class_transfer -- crates/`
//! finds no mount, no bin and no exec path), so what this falsifies is a documented CAPABILITY,
//! not a live code path. Why the three arguments that produced the old verdict were wrong is worth
//! keeping, because two of them are still true and only the conclusion did not follow:
//!   1. The Python SDK's `usd_class_transfer` does sign with `self.wallet` uniformly, with no
//!      master-only branch. TRUE, and it proves the opposite of what it was read to prove: the
//!      SDK's `account_address` is never consulted for this action either, so the SDK cannot
//!      express "this transfer is for my master" any more than we can. There is no field for it.
//!   2. The Rust SDK's `class_transfer` is signed with the **L1 phantom-agent** scheme
//!      (`sign_l1_action`) — the scheme agents genuinely do sign. TRUE as stated, and it is the
//!      one-time REOPENER here, and it is now **CLOSED — the L1 action no longer exists.** It read
//!      as a reopener because the premise is sound: an agent wallet's L1 action really does resolve
//!      to its MASTER at the venue (`crate::exec`'s `HyperliquidExecutionClient`), which is the
//!      opposite of the user-signed finding above — so if an L1 class transfer existed, the
//!      capability plausibly existed with it. MEASURED 2026-09-14 against the TESTNET `/exchange`
//!      by `crates/bridges/hyperliquid/tests/hyperliquid_l1_class_transfer_probe.rs`, posting the
//!      Rust SDK's own `{"type":"spotUser","classTransfer":{…}}` signed by `sign_l1_action`:
//!
//!      ```text
//!      HTTP 422  Failed to deserialize the JSON body into the target type
//!      ```
//!
//!      Refused on SHAPE, before signature verification — the venue does not know that action.
//!      Hyperliquid migrated class transfers to the user-signed form and the Rust SDK did not
//!      follow; the official API documentation describes only `usdClassTransfer`, and `spotUser`
//!      appears in it nowhere.
//!
//!      **The consequence is the whole answer to this file's question: an agent wallet cannot move
//!      a master's USDC between spot and perp by ANY scheme.** The L1 route would resolve to the
//!      master but does not exist; the user-signed route exists but resolves to the signer. What is
//!      NOT claimed: that an agent cannot act for its master on L1 generally — orders are L1 and do
//!      resolve to the master. This is one action's disappearance, not a permissions rule.
//!   3. Security model: funds LEAVING the account (`withdraw`/`usdSend`/`spotSend`) are master-only
//!      while `usdClassTransfer` keeps funds inside it, so it is not in that class. Plausible, and
//!      empirically not how the venue is built: the line it draws is not internal-vs-external but
//!      user-signed-vs-L1, and every user-signed action resolves to the signer's own account.
//!
//! # LIVE-VERIFIED on testnet — the SIGNING is proven, and so now is the PERMISSIONING
//! ⚠ This heading read **"the PERMISSIONING is not"** until the permissioning smoke was written;
//! before that the whole section read **"UNVERIFIED live … a real testnet round-trip is owed before
//! trusting it with funds"**. Both round-trips have now been made.
//! `crates/bridges/hyperliquid/tests/hyperliquid_user_signed_smoke.rs` posts THIS module's own
//! payload — through [`usd_class_transfer`], not a re-spelling of it — to the TESTNET `/exchange`,
//! and asserts the address Hyperliquid names back is the one the signing key derives. A user-signed
//! action carries no sender field, so the venue's answer is an ECDSA recovery over ITS OWN EIP-712
//! digest; an address that matches ours can only mean its digest is byte-identical to ours, which
//! certifies the `HyperliquidTransaction:UsdClassTransfer` type string character for character,
//! every field name (`toPerp` included) and type, the field ORDER inside the type string, the
//! domain and the chain id — in one assertion. MEASURED 2026-09-14: it matched.
//!
//! ⚠ **That smoke could not reach the PERMISSIONING, and this paragraph used to end by saying so.**
//! It signs with a key the venue has never seen, which is answered before any agent-approval lookup
//! could matter. The separate `crates/bridges/hyperliquid/tests/hyperliquid_permissioning_smoke.rs`
//! asks the same question with a signer the venue KNOWS, and its answer is the falsified verdict at
//! the top of this file. So both halves are now measured: the signing is right, and the agent may
//! not use it. What is still NOT settled is that no SDK publishes a golden VECTOR for this action —
//! still true, and the reason the guard is several files rather than one.
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
//! The tests below are unchanged and prove what they always did: (a) the exact wire shape and
//! field-order against the SDK, and (b) that the produced signature recovers the signer's own
//! address locally. Sub-account/vault transfers (the SDK's
//! `amount += " subaccount:{addr}"` form) are intentionally out of scope — this moves the signing
//! account's own USDC.

use serde::Serialize;
use serde_json::Value;

use vike_bridge_core::transport::VenueApiError;

use crate::config::Network;
use crate::consts::{USER_SIGNED_CHAIN_ID, VENUE};
use crate::signing::{Signature, Signer, eip712};
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
/// Build + sign the `usdClassTransfer` `/exchange` body for a FIXED `nonce` and return the EXACT
/// bytes [`usd_class_transfer`] posts — no network, no clock, no rate gate.
///
/// ⚠ **This exists so the wire shape is observable from OUTSIDE this file**, for the same reason
/// `transport::exchange_body` was split out. Every `#[serde(rename = "…")]` above is a protocol
/// constant: `toPerp` is the transfer's DIRECTION and `hyperliquidChain` is its replay fence, and
/// the venue re-derives the EIP-712 message from those keys. A guard for them that lives in this
/// file's own `mod tests` is not a guard — one editing pass rewrites the pin and its expected
/// literal together and every test stays green (measured in this repo on 2026-09-13, in
/// `crates/vike-datahub-client/src/proto.rs`). `tests/signed_payload_fixtures.rs` compares these
/// bytes against a frozen fixture under `fixtures/`, which a pass over `crates/` cannot reach.
pub fn usd_class_transfer_payload(
    signer: &Signer,
    amount: f64,
    to_perp: bool,
    network: Network,
    nonce: u64,
) -> Result<Vec<u8>, VenueApiError> {
    let (action, signature) = build_signed_action(signer, amount, to_perp, network, nonce);
    let body = TransferBody { action: &action, nonce, signature };
    // Serialized from the STRUCT (never via `serde_json::Value`, whose map is a `BTreeMap` in this
    // workspace's `serde_json` feature set) so the emitted key order stays the SDK's declaration
    // order rather than an alphabetical one.
    serde_json::to_vec(&body)
        .map_err(|e| VenueApiError { code: 0, msg: format!("bad transfer body: {e}") })
}

/// Both halves are testnet-verified against the venue: the SIGNING is right, and an AGENT wallet
/// may NOT use it for its master — the venue resolves a user-signed action to the SIGNER's own
/// account. Pass a MASTER key. See the module docs.
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
    let payload = usd_class_transfer_payload(signer, amount, to_perp, network, nonce)?;
    // A single action is IP weight 1 (`transport::exchange_weight(0)`); ride the same per-IP budget
    // window as orders/reads so a manual transfer can't overrun it.
    transport.rate_gate().proceed_cost_logged(VENUE, "usdClassTransfer", 1);
    let (_, exchange_url, _) = network.urls();
    transport.post(exchange_url, &payload)
}

#[cfg(test)]
mod tests {
    //! SDK-faithful wire shape + a self-consistency (recover-the-signer) proof. No golden vector —
    //! these pin the EIP-712 digest/round-trip and the exact JSON, nothing more. The VENUE's own
    //! verdict on the same payload is a separate, network-gated file (see the module doc's
    //! live-verified section); nothing here reaches it, and nothing here should be relaxed because
    //! it exists.
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
    /// the signer's own address. (This validates the crypto end-to-end LOCALLY; it does not by
    /// itself prove the VENUE computes the same digest — that is the network-gated smoke named in
    /// the module doc, and there is still no golden vector.)
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
