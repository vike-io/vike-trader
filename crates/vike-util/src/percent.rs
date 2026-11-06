//! RFC 3986 percent-encoding of one string (decision 0141): the ONE copy of the byte loop five
//! crates used to carry (the ctrader OAuth query, the IG Lightstreamer form and catalog search, the
//! Polymarket Gamma slug, the Databento query args).
//!
//! The contract, byte by byte over the UTF-8 encoding: the RFC 3986 UNRESERVED set
//! (`A-Z a-z 0-9 - . _ ~`) passes through literally, and every other byte, a space and each byte
//! of a multibyte character included, becomes `%XX` with UPPERCASE hex digits. There is no `+`
//! for a space: that is the `application/x-www-form-urlencoded` form, a different wire spelling,
//! which `vike_bridge_core::signer::urlencode` owns for the signed exchange queries. An encoder
//! that keeps a WIDER set (a `:` a reader wants to see in a logged URL, a path segment's
//! sub-delims) is a different function and stays with its caller.

/// Percent-encode `s`: the RFC 3986 unreserved bytes kept, everything else `%XX` in uppercase hex.
///
/// Byte-wise, so a multibyte UTF-8 character becomes one `%XX` per byte (`"é"` is `"%C3%A9"`).
pub fn percent_encode(s: &str) -> String {
    const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(b));
            }
            _ => {
                out.push('%');
                out.push(char::from(HEX_UPPER[usize::from(b >> 4)]));
                out.push(char::from(HEX_UPPER[usize::from(b & 0x0f)]));
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "percent_tests.rs"]
mod tests;
