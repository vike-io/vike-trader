//! Polymarket CLOB **V2** order construction + EIP-712 signing.
//!
//! Byte-for-byte transcribed from the official **`@polymarket/client` `0.1.0-beta.12`** SDK
//! (`exchange.ts`/`orders/*`) and LIVE-VALIDATED on Polygon mainnet (place+cancel accepted). beta.12
//! is verified protocol-identical to the `0.0.0-canary-20260608` build originally studied — across
//! exchange/wallet/post/orders/typed-data/authorization the ONLY delta is the GTD limit-order
//! minimum expiration (60s → 180s). That bound does not touch the SIGNING path (`expiration` is not
//! a signed field) — it is enforced client-side by [`crate::exec_plane::client::expiration_secs_of`], which
//! computes the `expiration` value [`order_to_json`] puts on the wire.
//!
//! V2 shape: domain version "2", NO taker/expiration/nonce/feeRateBps, plus `timestamp`(ms) /
//! `metadata` / `builder`. The domain NAME is ALWAYS "Polymarket CTF Exchange" (only the
//! verifyingContract switches for neg-risk). Deposit-wallet accounts sign the ERC-7739 wrapper
//! ([`sign_order_1271`]); everything reuses the LIVE-VALIDATED shared primitive,
//! [`eip712`](vike_bridge_core::eip712).

use vike_bridge_core::eip712::{
    digest, domain_separator, enc_address, enc_uint, enc_uint256_dec, hash_struct, keccak256,
    sign_digest_hex,
};

/// Order side (V2 numeric encoding).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn code(self) -> u128 {
        match self {
            Side::Buy => 0,
            Side::Sell => 1,
        }
    }
}

/// How the order is signed (V2 numeric encoding). Proxy/Safe wallets split maker (funder) vs signer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignatureType {
    Eoa,
    PolyProxy,
    PolyGnosisSafe,
    /// Deposit-wallet (the newest Polymarket account type): an ERC-1271 contract signature over an
    /// ERC-7739 `TypedDataSign` wrapper. maker == signer == the deposit wallet (see [`sign_order_1271`]).
    Poly1271,
}

impl SignatureType {
    pub fn code(self) -> u128 {
        match self {
            SignatureType::Eoa => 0,
            SignatureType::PolyProxy => 1,
            SignatureType::PolyGnosisSafe => 2,
            SignatureType::Poly1271 => 3,
        }
    }
}

const CHAIN_ID: u128 = 137;
// Per the official SDK the domain NAME is ALWAYS "Polymarket CTF Exchange" (even for neg-risk
// markets); only the verifyingContract switches. (The old NEG_RISK_NAME was a latent bug.)
const STD_NAME: &str = "Polymarket CTF Exchange";
const STD_CONTRACT: &str = "0xE111180000d2663C0091e4f400237545B87B996B";
const NEG_RISK_CONTRACT: &str = "0xe2222d279d744050d28e00520010520000310F59";
const ORDER_TYPE: &str = "Order(uint256 salt,address maker,address signer,uint256 tokenId,uint256 makerAmount,uint256 takerAmount,uint8 side,uint8 signatureType,uint256 timestamp,bytes32 metadata,bytes32 builder)";
// Deposit-wallet (POLY_1271) ERC-7739 wrapper — the EIP-712 struct that the deposit-wallet contract's
// isValidSignature expects. It nests the Order under a DepositWallet domain attestation.
const DEPOSIT_WALLET_DOMAIN_NAME: &str = "DepositWallet";
const DEPOSIT_WALLET_DOMAIN_VERSION: &str = "1";
const TYPED_DATA_SIGN_TYPE: &str = "TypedDataSign(Order contents,string name,string version,uint256 chainId,address verifyingContract,bytes32 salt)Order(uint256 salt,address maker,address signer,uint256 tokenId,uint256 makerAmount,uint256 takerAmount,uint8 side,uint8 signatureType,uint256 timestamp,bytes32 metadata,bytes32 builder)";

/// A V2 order ready to sign. `maker_amount`/`taker_amount` are 6-decimal base units; `token_id` is
/// the ERC-1155 outcome id (uint256 decimal string).
#[derive(Clone, Debug)]
pub struct Order {
    pub salt: u128,
    /// funder — the proxy/Safe address for POLY_PROXY/POLY_GNOSIS_SAFE, else the EOA.
    pub maker: String,
    /// the EOA that actually signs.
    pub signer: String,
    pub token_id: String,
    pub maker_amount: u128,
    pub taker_amount: u128,
    pub side: Side,
    pub signature_type: SignatureType,
    /// order timestamp, milliseconds (V2: replaces the V1 nonce).
    pub timestamp_ms: u128,
    /// app-defined bytes32, zero by default.
    pub metadata: [u8; 32],
    /// builderCode bytes32 for fee attribution, zero by default.
    pub builder: [u8; 32],
    /// multi-outcome (NegRisk) market → the NegRisk exchange domain/contract.
    pub neg_risk: bool,
}

fn order_domain(neg_risk: bool) -> [u8; 32] {
    let contract = if neg_risk { NEG_RISK_CONTRACT } else { STD_CONTRACT };
    domain_separator(STD_NAME, "2", CHAIN_ID, contract)
}

/// The EIP-712 struct hash for a V2 order.
pub fn order_struct_hash(o: &Order) -> [u8; 32] {
    hash_struct(
        ORDER_TYPE,
        &[
            enc_uint(o.salt),
            enc_address(&o.maker),
            enc_address(&o.signer),
            enc_uint256_dec(&o.token_id),
            enc_uint(o.maker_amount),
            enc_uint(o.taker_amount),
            enc_uint(o.side.code()),
            enc_uint(o.signature_type.code()),
            enc_uint(o.timestamp_ms),
            o.metadata,
            o.builder,
        ],
    )
}

/// The raw 32-byte EIP-712 order hash: `keccak256(0x1901 || domainSeparator || hashStruct(order))`,
/// the EXACT prehash [`sign_order`] signs. This is Polymarket's canonical order identity: the CLOB
/// rebuilds this same hash from the posted fields (which is why the live `place` smoke caps salt at
/// 53 bits, since a wider salt reshapes the hash and the server bounces the signature) and keys the
/// order by it, so the id is fixed by the order ALONE, knowable before submit and pre-signature.
pub fn order_id_hash(o: &Order) -> [u8; 32] {
    digest(&order_domain(o.neg_risk), &order_struct_hash(o))
}

/// The CLOB order id (`0x` + 64-hex, lowercase) of a V2 order, derived from the order pre-submit.
///
/// Equal to the venue's returned `orderID`: the CLOB echoes no client id and keys orders by the
/// EIP-712 order hash (registry.rs and the 2026-07-07 fill-lane design both record the server "id"
/// as the keccak order-hash, not the coid). Deriving it before the network ack lets the exec thread
/// pre-register `coid`↔`clob_id`, closing the ack-race window in which a user-WS fill can land
/// keyed by an id the registry does not yet hold (today's `client_order_id: None` path). That wiring
/// now exists in the submit path behind the DEFAULT-OFF `POLY_PRESUBMIT_REGISTER` gate
/// ([`crate::exec_plane::mount::presubmit_register_enabled`]); flipping it ON stays gated on the live smoke that
/// pins `derive_order_id(order) == resp["orderID"]` (`client.rs`'s `live_place_and_cancel`), because
/// booking a pre-registered id into the money path must be venue-proven first.
pub fn derive_order_id(o: &Order) -> String {
    format!("0x{}", hex::encode(order_id_hash(o)))
}

/// Sign a V2 order → hex signature (`0x…`, r||s||v). Signs exactly [`order_id_hash`].
pub fn sign_order(o: &Order, private_key: &str) -> Result<String, String> {
    sign_digest_hex(&order_id_hash(o), private_key)
}

/// Sign a **deposit-wallet (POLY_1271)** order → ERC-1271 wrapped hex signature.
///
/// The deposit-wallet contract validates an ERC-7739 `TypedDataSign` envelope, so we do NOT sign the
/// bare Order: we sign a `TypedDataSign` struct whose `contents` is the Order hash and whose message
/// attests the DepositWallet domain, then append the material the contract needs to reconstruct the
/// digest (`sig ‖ exchangeDomainSeparator ‖ contentsHash ‖ contentsType ‖ contentsTypeLen`). `o.signer`
/// MUST be the deposit wallet (== `o.maker`); the EOA `private_key` is the authorized signer the
/// contract recognises. Transcribed from `@polymarket/client` 0.1.0-beta.12 `exchange.ts`
/// (`createExchangeOrderTypedDataPayload`/`createExchangeOrderSignature`); live-validated on mainnet.
pub fn sign_order_1271(o: &Order, private_key: &str) -> Result<String, String> {
    let domain_sep = order_domain(o.neg_risk);
    let contents_hash = order_struct_hash(o);
    // hashStruct(TypedDataSign): `contents` (a referenced Order struct) encodes as the Order hash.
    let tds_hash = hash_struct(
        TYPED_DATA_SIGN_TYPE,
        &[
            contents_hash,
            keccak256(DEPOSIT_WALLET_DOMAIN_NAME.as_bytes()),
            keccak256(DEPOSIT_WALLET_DOMAIN_VERSION.as_bytes()),
            enc_uint(CHAIN_ID),
            enc_address(&o.signer), // verifyingContract = the deposit wallet (== signer for 1271)
            [0u8; 32],              // salt
        ],
    );
    let sig = sign_digest_hex(&digest(&domain_sep, &tds_hash), private_key)?;
    let sig = sig.trim_start_matches("0x");
    // ERC-1271 wrapper: raw sig ‖ exchange domain separator ‖ Order hash ‖ Order typestring ‖ len(2B).
    Ok(format!(
        "0x{sig}{}{}{}{:04x}",
        hex::encode(domain_sep),
        hex::encode(contents_hash),
        hex::encode(ORDER_TYPE.as_bytes()),
        ORDER_TYPE.len(),
    ))
}

/// The zero address — a public order's `taker`.
pub const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

/// Decimal quantity → Polymarket 6-decimal base units.
fn to_base_units(x: f64) -> u128 {
    (x * 1_000_000.0).round() as u128
}

/// Build a V2 order from a `price` (probability, assumed tick-aligned) + `size` (shares). BUY gives
/// USDC for shares; SELL gives shares for USDC. Per-side rounding to keep the effective price exactly
/// on-tick is a refinement validated live.
#[expect(clippy::too_many_arguments)]
pub fn build_order(
    price: f64,
    size: f64,
    side: Side,
    token_id: &str,
    maker: &str,
    signer: &str,
    signature_type: SignatureType,
    timestamp_ms: u128,
    salt: u128,
    neg_risk: bool,
    builder: [u8; 32], // builderCode for fee attribution (zero = unattributed, as before)
) -> Order {
    let shares = to_base_units(size);
    let usdc = to_base_units(price * size);
    let (maker_amount, taker_amount) = match side {
        Side::Buy => (usdc, shares),
        Side::Sell => (shares, usdc),
    };
    Order {
        salt,
        maker: maker.to_string(),
        signer: signer.to_string(),
        token_id: token_id.to_string(),
        maker_amount,
        taker_amount,
        side,
        signature_type,
        timestamp_ms,
        metadata: [0u8; 32],
        builder,
        neg_risk,
    }
}

/// The signed order as the POST `/order` `order` object (the client wraps it with owner/orderType).
///
/// `expiration_secs` is the wire `expiration` in **unix SECONDS**, and `0` means "no expiry" — the
/// value every GTC/FOK order sends. A GTD order MUST pass a real deadline here: the caller computes
/// it with [`crate::exec_plane::client::expiration_secs_of`], the sibling of `order_type_of`. It is an explicit
/// parameter rather than a defaulted field precisely so no call site can ship a GTD order with the
/// GTC `"0"` by omission (which is exactly the bug this signature replaced).
///
/// ⚠ `expiration` is **outside the EIP-712 signed struct** — V2 dropped it from [`ORDER_TYPE`], so
/// [`order_struct_hash`] never reads it and changing it does NOT move the signature preimage or the
/// derived [`derive_order_id`]. That is why this field can be corrected without re-validating any
/// signature golden.
pub fn order_to_json(o: &Order, signature: &str, expiration_secs: u64) -> serde_json::Value {
    // V2 wire (matches the official SDK dict): salt + signatureType are JSON NUMBERS; the amounts/
    // tokenId stay strings (big uint256s); `expiration` REMAINS in the wire (string, "0" for
    // GTC/FOK — the endpoint's orderType drives expiry) though V2 dropped it from the SIGNED
    // struct; no taker.
    serde_json::json!({
        "salt": o.salt as u64,
        // lowercase — Polymarket's CLOB matches the funder by case-sensitive string
        "maker": o.maker.to_lowercase(),
        "signer": o.signer.to_lowercase(),
        "tokenId": o.token_id,
        "makerAmount": o.maker_amount.to_string(),
        "takerAmount": o.taker_amount.to_string(),
        "side": match o.side { Side::Buy => "BUY", Side::Sell => "SELL" },
        "expiration": expiration_secs.to_string(),
        "signatureType": o.signature_type.code(),
        "timestamp": o.timestamp_ms.to_string(),
        "metadata": format!("0x{}", hex::encode(o.metadata)),
        "builder": format!("0x{}", hex::encode(o.builder)),
        "signature": signature,
    })
}

#[path = "order_tests.rs"]
#[cfg(test)]
mod order_tests;
