//! Hash bytes to lowercase hex: the ONE home for "SHA-256 of these bytes, as a hex string".
//!
//! The digests it produces are PERSISTED and COMPARED — the tradehub's script-mount audit line
//! (which source traded) and the starter dataset's `SHA256SUMS` check — so the spelling is part
//! of the contract: lowercase, two digits per byte, no prefix, no separator. `hash_tests.rs` pins
//! it against published vectors.
//!
//! It lives in this crate because the crate already links `sha2` (the handshake's HMAC) and names
//! no `vike-*` crate, so every daemon that hashes sits above it at no new edge; the helper floor
//! (`vike_model::util` then, `vike-util` now) was refused for it because it does not link `sha2`
//! (`docs/decisions/0141-vike-model-admits-one-util-module-for-helpers-two-crates-copy.md`).
//! `lower_hex` is the one hex loop: `crate::auth`'s `key_fingerprint` encodes its truncated tag
//! through it too.

use sha2::{Digest, Sha256};

/// Lowercase hex SHA-256 of `bytes`: 64 characters, `[0-9a-f]` only.
pub fn sha256_hex(bytes: &[u8]) -> String {
    lower_hex(&Sha256::digest(bytes))
}

/// `bytes` as lowercase hex, two digits per byte, in order.
pub(crate) fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(HEX[usize::from(b >> 4)]));
        out.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
    out
}

#[cfg(test)]
#[path = "hash_tests.rs"]
mod tests;
