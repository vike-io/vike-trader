//! Hyperliquid action signing — the existential core (a wrong byte silently rejects orders).
//!
//! Two schemes. The L1 (phantom-agent) scheme signs orders/cancels/modify; the **user-signed**
//! scheme (domain `HyperliquidSignTransaction`, chainId `0x66eee`) now signs `usdClassTransfer` (see
//! [`crate::transfer`] + [`eip712::sign_usd_class_transfer`]) — other transfers/approvals slot in the
//! same way. [`eip712`] carries the primitives for both.
//!
//! ## Contract (what the fan-out implements)
//! - [`action`] — the ordered `#[serde(tag = "type", rename_all = "camelCase")]` action enum + the
//!   order/cancel/modify wire structs. **Field order IS the signature**: order fields serialize
//!   `a, b, p, s, r, t, c`; the envelope emits `type` first, then `orders`, `grouping`, `builder?`.
//!   `#[serde(skip_serializing_if = "Option::is_none")]` on every optional (`c`, `builder`) so
//!   absent ≠ null; cancel `f` / modify `a` flags are omitted when false (the venue rejects a hash
//!   containing `f: false`).
//! - [`hash`] — `keccak256( rmp_serde::to_vec_named(action) ‖ nonce.to_be_bytes() ‖ vault_marker ‖
//!   [expiresAfter] )`, where `vault_marker = 0x00` | `0x01 ‖ addr[20]`, `expiresAfter` leg =
//!   `0x00 ‖ u64_be`. Produces the `connectionId`.
//! - [`eip712`] — sign `Agent { source: "a"|"b", connectionId }` under domain `("Exchange", "1",
//!   chainId 1337, verifyingContract 0x0)` → `{ r, s, v }` with `v = 27 + recid`. Reuses the flat
//!   EIP-712 primitives re-exported from `vike_bridge_core::eip712` (the ONE shared copy — the
//!   former verbatim local twin of `vike-polymarket`'s was deleted).
//! - [`NonceManager`] — a monotonic per-signer millisecond counter (`next = max(now_ms, last + 1)`,
//!   fast-forwarded to now if far behind), so same-ms bursts never collide.
//!
//! Correctness gate: `tests/signing_vectors.rs` asserts byte-exact signatures against the official
//! Rust SDK's hardcoded vectors. A green gate here is the precondition for any live order.

pub mod action;
pub mod eip712;
pub mod hash;

pub use hash::{NonceManager, action_hash};

use k256::ecdsa::SigningKey;
use serde::Serialize;

use crate::config::Network;

/// A signed L1 action's signature in HL wire shape — serializes to `{"r":"0x…","s":"0x…","v":28}`,
/// the object `/exchange` bodies carry alongside `action`/`nonce`/`vaultAddress`. `r`/`s` are
/// `0x`-prefixed 32-byte hex (padded); `v` is `27 | 28` (`27 + recovery_id`), matching the Python
/// SDK's `sign_inner` (`{"r": to_hex(r), "s": to_hex(s), "v": v}`).
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    pub r: String,
    pub s: String,
    pub v: u8,
}

/// The signing facade exec/recon build orders through: a parsed secp256k1 key + its derived
/// lowercased `0x`-address + the network (which fixes the phantom-agent `source` byte and the L1
/// chain id). Construct once per credential; `sign_l1_action` is then infallible (the key was
/// validated at construction) and cheap enough for the per-order boundary.
///
/// The key is a secret: `Signer` deliberately has **no** `Debug`/`Display`/`Clone` (nothing to
/// accidentally log or copy the material through), and the raw bytes never leave this type.
pub struct Signer {
    key: SigningKey,
    address: String,
    network: Network,
}

impl Signer {
    /// Parse a secp256k1 private key (`0x`-optional hex) for `network`, deriving and caching the
    /// signer's lowercased `0x` EOA address. `Err` if the hex is malformed or not a valid key —
    /// the ONLY fallible step; everything downstream (`sign_l1_action`) is then infallible.
    ///
    /// When the key is an **agent (API) wallet**, this derived address is the agent's — the master
    /// account address is carried separately ([`crate::config::HlCredentials::account_address`]) and
    /// used for `/info` reads, never for signing.
    pub fn from_private_key(hex: &str, network: Network) -> Result<Self, String> {
        let raw = hex.strip_prefix("0x").unwrap_or(hex);
        let bytes = hex::decode(raw).map_err(|e| format!("private key is not valid hex: {e}"))?;
        let key =
            SigningKey::from_slice(&bytes).map_err(|e| format!("invalid secp256k1 key: {e}"))?;
        let address = eip712::eth_address_from_private_key(hex)?;
        Ok(Self { key, address, network })
    }

    /// The signer's derived EOA address (lowercased `0x…`).
    pub fn address(&self) -> &str {
        &self.address
    }

    /// This signer's network (mainnet/testnet) — the source of the phantom-agent `source` byte.
    pub fn network(&self) -> Network {
        self.network
    }

    /// Sign an L1 action (order/cancel/modify): [`action_hash`] → phantom `Agent { source, connectionId }`
    /// under the Exchange domain → `{r, s, v}`. `vault`/`expires_after` are `None` in v1.
    ///
    /// Infallible: the key was validated in [`Signer::from_private_key`], and secp256k1 signing over
    /// a fixed 32-byte digest with a valid key cannot fail.
    pub fn sign_l1_action(
        &self,
        action: &impl Serialize,
        nonce: u64,
        vault: Option<[u8; 20]>,
        expires_after: Option<u64>,
    ) -> Signature {
        let connection_id = action_hash(action, nonce, vault, expires_after);
        let digest = eip712::agent_signing_digest(
            self.network.phantom_source(),
            &connection_id,
            crate::consts::EXCHANGE_CHAIN_ID as u128,
        );
        let (sig, recid) = self.key.sign_prehash_recoverable(&digest);
        let b = sig.to_bytes(); // r(32) ‖ s(32), low-S normalized
        Signature {
            r: format!("0x{}", hex::encode(&b[..32])),
            s: format!("0x{}", hex::encode(&b[32..])),
            v: recid.to_byte() + 27,
        }
    }
}
