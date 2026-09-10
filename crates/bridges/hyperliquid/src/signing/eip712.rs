//! Minimal EIP-712 typed-data hashing + secp256k1 signing for Hyperliquid — the byte-exact core
//! (a wrong field silently rejects an order).
//!
//! The generic primitive block (keccak256, `enc_address`/`enc_uint`/`enc_string`, flat
//! `hashStruct`/`typeHash`, the domain separator, the `\x19\x01` digest, recoverable `k256`
//! signing, EOA derivation) is **re-exported from [`vike_bridge_core::eip712`]** — the ONE shared
//! primitive (behind bridge-core's `eip712` feature, enabled by this crate's Cargo.toml), which
//! carries the canonical EIP-712 spec-vector gate (keccak("abc"), the "Ether Mail" domain
//! separator, the reference "Mail" signature, the "Cow" EOA). The former verbatim local copy —
//! the spec's original "no shared crate for two call sites" reuse decision — was deleted once a
//! THIRD copy appeared; the re-export keeps every `signing::eip712::…` path stable. Only FLAT
//! structs are supported (address / uintN / string / bytes32 fields); a `bytes32` field is
//! already a 32-byte word and is passed to [`hash_struct`] unchanged — which is how the phantom
//! `Agent`'s `connectionId` is encoded. [`sign_digest`]'s low-S-normalized `r||s||v` matches
//! alloy, which produced the golden vectors, so the RFC-6979 deterministic `(r, s, recid)` is
//! byte-identical.
//!
//! On top of those primitives sit the **HL-specific schemes**: signing the phantom
//! [`agent_struct_hash`] `Agent(string source,bytes32 connectionId)` under the L1 domain
//! `("Exchange", "1", chainId 1337, verifyingContract 0x0)` → `{ r, s, v = 27 + recid }`
//! ([`sign_agent`]), and the user-signed scheme (domain `HyperliquidSignTransaction`, chainId
//! `0x66eee`) — a second, differently-named domain over the SAME [`domain_separator`] /
//! [`hash_struct`] / [`digest`] primitives ([`usd_class_transfer_digest`] /
//! [`sign_usd_class_transfer`], [`approve_builder_fee_digest`] / [`sign_approve_builder_fee`]).

use k256::ecdsa::{RecoveryId, Signature};

// The shared primitive block — ONE copy, golden-vector-gated in vike-bridge-core (see module
// doc). ⚠ Judged API by the author, 2026-08-18 — the crate's signing VOCABULARY, deliberately
// kept under the no-move-shims convention (root CLAUDE.md): the HL-specific schemes below and
// these primitives read as ONE surface at every call site. Not a move shim; do not re-flag.
// Re-exported under the old paths so `signing::hash`, `signing::Signer`, the transfer/
// builder-fee modules, and the SDK-vector tests keep resolving `signing::eip712::…` unchanged.
pub use vike_bridge_core::eip712::{
    digest, domain_separator, enc_address, enc_string, enc_uint, eth_address_from_private_key,
    hash_struct, keccak256, sign_digest,
};

/// The all-zero `verifyingContract` used by BOTH HL domains (L1 phantom-agent and the user-signed
/// transaction domain).
pub const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

// ---- Hyperliquid phantom-agent (L1) additions ------------------------------------------------

/// `hashStruct` of HL's phantom `Agent(string source,bytes32 connectionId)`: the `source` byte
/// (`"a"` mainnet / `"b"` testnet) is a `string` (keccak-hashed), the `connectionId` (the action
/// hash) is a `bytes32` passed through verbatim.
pub fn agent_struct_hash(source: &str, connection_id: &[u8; 32]) -> [u8; 32] {
    hash_struct("Agent(string source,bytes32 connectionId)", &[enc_string(source), *connection_id])
}

/// The full `\x19\x01`-prefixed signing digest for an L1 phantom agent under the Exchange domain
/// `("Exchange", "1", chain_id, 0x0)`. `chain_id` is always [`crate::consts::EXCHANGE_CHAIN_ID`]
/// (`1337`) for L1 — a fixed value independent of the wallet's chain; passed as a parameter so the
/// primitive stays free of a `consts` dependency and is unit-testable in isolation.
pub fn agent_signing_digest(source: &str, connection_id: &[u8; 32], chain_id: u128) -> [u8; 32] {
    let domain_sep = domain_separator("Exchange", "1", chain_id, ZERO_ADDRESS);
    digest(&domain_sep, &agent_struct_hash(source, connection_id))
}

/// Sign an L1 phantom agent → 65-byte `r‖s‖v` (v = 27 + recid). The standalone (hex-key) entry
/// point used by the golden-vector tests; [`crate::signing::Signer`] uses [`agent_signing_digest`]
/// directly with its pre-parsed key.
pub fn sign_agent(
    source: &str,
    connection_id: &[u8; 32],
    chain_id: u128,
    private_key_hex: &str,
) -> Result<[u8; 65], String> {
    sign_digest(&agent_signing_digest(source, connection_id, chain_id), private_key_hex)
}

// ---- Hyperliquid user-signed action additions (usdClassTransfer) -------------------------------
//
// A SECOND scheme, distinct from the L1 phantom-agent one above: instead of hashing the msgpack
// action + signing a phantom `Agent`, the *fields themselves* are the EIP-712 message under the
// `HyperliquidSignTransaction` domain. Ported field-for-field from the official Python SDK
// (`signing.py::{sign_usd_class_transfer_action, USD_CLASS_TRANSFER_SIGN_TYPES, user_signed_payload}`).
// SDK-spec-faithful but UNVERIFIED live (no golden vector) — see `crate::transfer`.

/// `hashStruct` of HL's user-signed `HyperliquidTransaction:UsdClassTransfer(string hyperliquidChain,
/// string amount,bool toPerp,uint64 nonce)`. EIP-712 field encodings: the two `string`s are
/// keccak-hashed ([`enc_string`]); `toPerp` (a `bool`) encodes as a `uint256` `0`/`1`; `nonce`
/// (`uint64`) as a `uint256` — both via [`enc_uint`]. Field order is the signed order (load-bearing).
pub fn usd_class_transfer_struct_hash(
    hyperliquid_chain: &str,
    amount: &str,
    to_perp: bool,
    nonce: u64,
) -> [u8; 32] {
    hash_struct(
        "HyperliquidTransaction:UsdClassTransfer(string hyperliquidChain,string amount,bool toPerp,uint64 nonce)",
        &[
            enc_string(hyperliquid_chain),
            enc_string(amount),
            enc_uint(u128::from(to_perp)),
            enc_uint(u128::from(nonce)),
        ],
    )
}

/// The full `\x19\x01`-prefixed signing digest for a usdClassTransfer under the user-signed domain
/// `("HyperliquidSignTransaction", "1", signature_chain_id, 0x0)`. `signature_chain_id` is HL's fixed
/// `0x66eee` (`421614`, [`crate::consts::USER_SIGNED_CHAIN_ID`]); passed as a parameter so this
/// primitive stays free of a `consts` dependency and is unit-testable in isolation (mirrors
/// [`agent_signing_digest`]).
pub fn usd_class_transfer_digest(
    hyperliquid_chain: &str,
    amount: &str,
    to_perp: bool,
    nonce: u64,
    signature_chain_id: u128,
) -> [u8; 32] {
    let domain_sep =
        domain_separator("HyperliquidSignTransaction", "1", signature_chain_id, ZERO_ADDRESS);
    digest(&domain_sep, &usd_class_transfer_struct_hash(hyperliquid_chain, amount, to_perp, nonce))
}

/// Sign a usdClassTransfer with a [`super::Signer`]'s key → HL wire `{r,s,v}` (v = 27 + recid, low-S
/// normalized) — the user-signed twin of [`super::Signer::sign_l1_action`].
///
/// Lives here (rather than as a `Signer` method) so the whole user-signed scheme sits beside its
/// digest, and because `eip712` is a descendant module of `signing`: it can read the `Signer`'s
/// otherwise-encapsulated private `key`, so the secret still never leaves the `signing` module.
/// Infallible for the same reason as the L1 path (validated key, fixed 32-byte digest).
pub fn sign_usd_class_transfer(
    signer: &super::Signer,
    hyperliquid_chain: &str,
    amount: &str,
    to_perp: bool,
    nonce: u64,
    signature_chain_id: u128,
) -> super::Signature {
    let d =
        usd_class_transfer_digest(hyperliquid_chain, amount, to_perp, nonce, signature_chain_id);
    let (sig, recid): (Signature, RecoveryId) = signer.key.sign_prehash_recoverable(&d);
    let b = sig.to_bytes(); // r(32) ‖ s(32), low-S normalized
    super::Signature {
        r: format!("0x{}", hex::encode(&b[..32])),
        s: format!("0x{}", hex::encode(&b[32..])),
        v: recid.to_byte() + 27,
    }
}

// ---- Hyperliquid user-signed action additions (approveBuilderFee) ------------------------------
//
// A THIRD scheme instance of the same user-signed pattern as `usdClassTransfer` above (SAME
// `HyperliquidSignTransaction` domain, different `primaryType`). Ported field-for-field from the
// official Python SDK (`signing.py::{sign_approve_builder_fee, APPROVE_BUILDER_FEE_SIGN_TYPES}`).
// SDK-spec-faithful but UNVERIFIED live (no golden vector) — see `crate::builder_fee`.

/// `hashStruct` of HL's user-signed `HyperliquidTransaction:ApproveBuilderFee(string
/// hyperliquidChain,string maxFeeRate,address builder,uint64 nonce)`. Field order is the signed
/// order (load-bearing): the two `string`s keccak-hashed ([`enc_string`]), `builder` as an
/// `address` ([`enc_address`]), `nonce` (`uint64`) as a `uint256` ([`enc_uint`]).
pub fn approve_builder_fee_struct_hash(
    hyperliquid_chain: &str,
    max_fee_rate: &str,
    builder: &str,
    nonce: u64,
) -> [u8; 32] {
    hash_struct(
        "HyperliquidTransaction:ApproveBuilderFee(string hyperliquidChain,string maxFeeRate,address builder,uint64 nonce)",
        &[
            enc_string(hyperliquid_chain),
            enc_string(max_fee_rate),
            enc_address(builder),
            enc_uint(u128::from(nonce)),
        ],
    )
}

/// The full `\x19\x01`-prefixed signing digest for an `approveBuilderFee` under the SAME
/// user-signed domain as [`usd_class_transfer_digest`] (`("HyperliquidSignTransaction", "1",
/// signature_chain_id, 0x0)`).
pub fn approve_builder_fee_digest(
    hyperliquid_chain: &str,
    max_fee_rate: &str,
    builder: &str,
    nonce: u64,
    signature_chain_id: u128,
) -> [u8; 32] {
    let domain_sep =
        domain_separator("HyperliquidSignTransaction", "1", signature_chain_id, ZERO_ADDRESS);
    digest(
        &domain_sep,
        &approve_builder_fee_struct_hash(hyperliquid_chain, max_fee_rate, builder, nonce),
    )
}

/// Sign an `approveBuilderFee` with a [`super::Signer`]'s key → HL wire `{r,s,v}` — the
/// `approveBuilderFee` twin of [`sign_usd_class_transfer`]. **Must be signed by the MASTER
/// account's own key** (HL requires master-account approval for a builder to charge a nonzero
/// fee — an agent/API wallet's signature is accepted for orders but not for this grant); the
/// caller is responsible for passing a `Signer` built from the master private key, not an agent's.
pub fn sign_approve_builder_fee(
    signer: &super::Signer,
    hyperliquid_chain: &str,
    max_fee_rate: &str,
    builder: &str,
    nonce: u64,
    signature_chain_id: u128,
) -> super::Signature {
    let d = approve_builder_fee_digest(
        hyperliquid_chain,
        max_fee_rate,
        builder,
        nonce,
        signature_chain_id,
    );
    let (sig, recid): (Signature, RecoveryId) = signer.key.sign_prehash_recoverable(&d);
    let b = sig.to_bytes(); // r(32) ‖ s(32), low-S normalized
    super::Signature {
        r: format!("0x{}", hex::encode(&b[..32])),
        s: format!("0x{}", hex::encode(&b[32..])),
        v: recid.to_byte() + 27,
    }
}

// No unit tests here: the generic primitive block keeps its golden-vector gate (the canonical
// EIP-712 spec vectors) in `vike-bridge-core/src/eip712.rs` — the four verbatim copies this module
// used to carry were deleted with the copy of the primitives themselves. The HL-specific L1 path
// (Exchange domain + phantom `Agent` + secp256k1 sign) is validated END-TO-END against the
// official Rust SDK's hardcoded signatures in `tests/signing_vectors.rs` — including the SDK's
// fixed-`connectionId` `sign_l1_action` vector, which pins THIS module's
// `sign_agent`/`agent_signing_digest`/domain-separator bytes independently of msgpack. No
// fabricated expected values live here.
