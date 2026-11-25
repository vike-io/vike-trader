//! Hash bytes to lowercase hex: the ONE home for "SHA-256 of these bytes, as a hex string".
//!
//! The digests it produces are PERSISTED and COMPARED — the tradehub's script-mount audit line
//! (which source traded) and the starter dataset's `SHA256SUMS` check — so the spelling is part
//! of the contract: lowercase, two digits per byte, no prefix, no separator. `hash_tests.rs` pins
//! it against published vectors.
//!
//! It lives in this crate because the crate already links `sha2` (the handshake's HMAC) and names
//! no domain crate, so every daemon that hashes sits above it at no new edge; the helper floor
//! (`vike_model::util` then, `vike-utility` now) was refused for it because it does not link `sha2`
//! (`docs/decisions/0141-vike-model-admits-one-util-module-for-helpers-two-crates-copy.md`). The hex
//! half is that floor's: `vike_utility::hex::hex_lower`, the workspace's one lowercase hex, which
//! `crate::auth`'s `key_fingerprint` encodes its truncated tag through too.

use sha2::{Digest, Sha256};

/// Lowercase hex SHA-256 of `bytes`: 64 characters, `[0-9a-f]` only.
pub fn sha256_hex(bytes: &[u8]) -> String {
    vike_utility::hex::hex_lower(&Sha256::digest(bytes))
}

/// [`sha256_hex`] of the file at `path`, read in parts: a program is hundreds of megabytes, never one allocation. What a
/// daemon names the program it runs by (the backtesthub's answer from a saved run, the datahub's `Welcome` build).
///
/// # Errors
/// The file's open or read failure.
pub fn sha256_hex_of_file(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let (mut hasher, mut part) = (Sha256::new(), vec![0u8; 1 << 20]);
    loop {
        match file.read(&mut part)? {
            0 => return Ok(vike_utility::hex::hex_lower(&hasher.finalize())),
            n => hasher.update(&part[..n]),
        }
    }
}

#[cfg(test)]
#[path = "hash_tests.rs"]
mod tests;
