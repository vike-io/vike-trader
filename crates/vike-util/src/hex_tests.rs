//! Golden outputs of [`super::hex_lower`], pinned to what the deleted nibble-table loops and the
//! `format!("{b:02x}")` folds returned.

use super::hex_lower;

#[test]
fn the_empty_slice_is_the_empty_string() {
    assert_eq!(hex_lower(&[]), "");
}

#[test]
fn edge_bytes_are_two_zero_padded_lowercase_digits() {
    assert_eq!(hex_lower(&[0x00]), "00");
    assert_eq!(hex_lower(&[0xff]), "ff");
    assert_eq!(hex_lower(&[0x0f]), "0f");
    assert_eq!(hex_lower(&[0xf0]), "f0");
}

#[test]
fn a_mixed_slice_keeps_byte_order() {
    assert_eq!(hex_lower(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    assert_eq!(hex_lower(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]), "0123456789abcdef");
    assert_eq!(hex_lower(&[0x00, 0x7f, 0x80, 0xff]), "007f80ff");
}

#[test]
fn every_byte_agrees_with_the_format_fold_it_replaced() {
    let all: Vec<u8> = (0..=255u8).collect();
    let folded: String = all.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex_lower(&all), folded);
    assert_eq!(hex_lower(&all).len(), 512);
}
