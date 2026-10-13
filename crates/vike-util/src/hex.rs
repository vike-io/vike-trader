//! Lowercase hex of a byte slice (decision 0141): the ONE copy of the nibble loop several crates
//! carried (a node key, a Telegram confirm token, a coid session prefix, the hex half of each
//! sha256 digest helper).
//!
//! The contract: two lowercase hex digits per byte, most significant nibble first, no prefix and no
//! separator, so the output is exactly `2 * bytes.len()` ASCII characters. It is what
//! `bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()` returns, without a `format!` per
//! byte. Hashing is not here: a sha256-to-hex helper needs `sha2`, which this crate does not link
//! (decision 0141's verdict 5), so each one keeps its digest and calls this for the hex.

/// `bytes` as lowercase hex, two digits per byte (`[0x00, 0xff]` is `"00ff"`).
pub fn hex_lower(bytes: &[u8]) -> String {
    const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(char::from(HEX_LOWER[usize::from(b >> 4)]));
        out.push(char::from(HEX_LOWER[usize::from(b & 0x0f)]));
    }
    out
}

#[cfg(test)]
#[path = "hex_tests.rs"]
mod tests;
