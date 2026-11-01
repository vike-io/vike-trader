use super::*;

/// The values the deleted copies produced, pinned as values: every one of them is persisted
/// somewhere (`trials.jsonl`, an incident manifest, a journal `Snap`'s `state_hash`, a settlement
/// fill id), so a changed digit is a changed identity, not a refactor.
const GOLDEN: &[(&[u8], &str)] = &[
    // The offset basis: `crates/vike-backtest/src/trial_ledger_tests.rs` and the incident
    // collector's tests pinned this one.
    (b"", "cbf29ce484222325"),
    // The published FNV-1a 64 test vectors.
    (b"a", "af63dc4c8601ec8c"),
    (b"foobar", "85944171f73967e8"),
    (b"\0", "af63bd4c8601b7df"),
    // Every byte value class: a 0x00, a 0xff (no sign extension), 0x80, 0x7f.
    (b"\xff", "af64724c8602eb6e"),
    (b"\x00\xff\x80\x7f", "d865f07bf62911ed"),
    (b"abc", "e71fa2190541574b"),
    // A profile body (trial ledger) and a profile row (incident collector).
    (b"cash = 10000.0", "3e7408f311b7a168"),
    (b"profile-a", "51ef31f3b18432c8"),
    // Canonical-JSON bytes, the shape `vike_exec::state_hash` and the conditional books' hash feed.
    (br#"{"a":1}"#, "9c3e82dd6fcae8b1"),
    // A Hyperliquid settlement seed, `<outcome>|<side>|<wallet lowercased>`.
    (b"3151|0|0xabcdef0123456789abcdef0123456789abcdef01", "8ce779e322dfe38d"),
    (b"hello world", "779a65e7023cd2e7"),
];

#[test]
fn hex_is_the_pinned_sixteen_lowercase_digits() {
    for (input, want) in GOLDEN {
        assert_eq!(fnv1a64_hex(input), *want, "fnv1a64_hex({input:?})");
        assert_eq!(fnv1a64(input), u64::from_str_radix(want, 16).unwrap(), "fnv1a64({input:?})");
    }
    // Zero-padded: a digest with a leading zero nibble still renders 16 digits.
    assert_eq!(format!("{:016x}", 0x0123_u64), "0000000000000123");
}

/// The deleted copies, verbatim in shape: one byte loop over the offset basis. The home must agree
/// with it on every input, not only on the golden ones.
fn reference(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[test]
fn agrees_with_the_byte_loop_it_replaced_on_every_length() {
    let bytes: Vec<u8> = (0..=255u8).chain((0..=255u8).rev()).collect();
    for n in 0..bytes.len() {
        assert_eq!(fnv1a64(&bytes[..n]), reference(&bytes[..n]), "prefix of {n} bytes");
    }
}

#[test]
fn pieces_hash_as_their_concatenation() {
    let mut h = Fnv1a64::new();
    h.write(b"foo");
    h.write(b"");
    h.write(b"bar");
    assert_eq!(h.finish(), fnv1a64(b"foobar"));
    assert_eq!(Fnv1a64::default().finish(), fnv1a64(b""));

    // The accounts-epoch shape: sorted keys, each followed by a NUL terminator.
    let mut e = Fnv1a64::new();
    for key in ["a", "b"] {
        e.write(key.as_bytes());
        e.write(&[0]);
    }
    assert_eq!(format!("{:016x}", e.finish()), "ab40d7820d408076");
    assert_eq!(e.finish(), fnv1a64(b"a\0b\0"));

    // The fill-print shape: fixed-width little-endian fields.
    let mut f = Fnv1a64::new();
    f.write(b"BTCUSDT");
    f.write(&1i64.to_le_bytes());
    f.write(&0.5f64.to_bits().to_le_bytes());
    let mut joined = b"BTCUSDT".to_vec();
    joined.extend_from_slice(&1i64.to_le_bytes());
    joined.extend_from_slice(&0.5f64.to_bits().to_le_bytes());
    assert_eq!(f.finish(), reference(&joined));
}
