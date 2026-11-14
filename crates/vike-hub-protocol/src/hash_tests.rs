//! Golden pins for `hash`: the exact strings the replaced copies produced (the tradehub's
//! `script_sha256` and the starter download's `sha256_hex`, both a `{b:02x}` fold over `sha2`'s
//! digest), against vectors published outside this codebase.

use super::{lower_hex, sha256_hex};

/// FIPS 180-2's SHA-256 of the empty input.
#[test]
fn the_empty_input_hashes_to_the_published_vector() {
    assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
}

/// FIPS 180-2's "abc" vector — the one the tradehub's script audit test pinned.
#[test]
fn abc_hashes_to_the_published_vector() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

/// FIPS 180-2's two-block vector, so a digest longer than one compression block is pinned too.
#[test]
fn the_two_block_vector_hashes_to_the_published_value() {
    assert_eq!(
        sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
}

/// The shape every persisted digest has: 64 lowercase hex digits, distinct inputs distinct.
#[test]
fn a_digest_is_64_lowercase_hex_digits() {
    let hex = sha256_hex(b"vike");
    assert_eq!(hex.len(), 64);
    assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{hex}");
    assert_ne!(sha256_hex(b"a"), sha256_hex(b"b"));
}

/// The hex loop alone: zero-padded, lowercase, in byte order, empty for empty.
#[test]
fn lower_hex_pads_and_lowercases_every_byte() {
    assert_eq!(lower_hex(&[]), "");
    assert_eq!(lower_hex(&[0x00, 0x0f, 0xa0, 0xff, 0x7e]), "000fa0ff7e");
}
