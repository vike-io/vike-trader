//! Golden outputs of [`super::percent_encode`], pinned to what the five deleted copies returned
//! (`format!("%{b:02X}")` over every byte outside `A-Za-z0-9-._~`).

use super::percent_encode;

#[test]
fn the_unreserved_set_passes_through() {
    let unreserved = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    assert_eq!(percent_encode(unreserved), unreserved);
}

#[test]
fn the_empty_string_stays_empty() {
    assert_eq!(percent_encode(""), "");
}

#[test]
fn a_space_is_percent_20_never_plus() {
    assert_eq!(percent_encode(" "), "%20");
    assert_eq!(percent_encode("CONFIRMS OPU WOU"), "CONFIRMS%20OPU%20WOU");
}

#[test]
fn reserved_bytes_are_uppercase_hex() {
    assert_eq!(percent_encode("/"), "%2F");
    assert_eq!(percent_encode("+"), "%2B");
    assert_eq!(percent_encode("http://localhost:5033/"), "http%3A%2F%2Flocalhost%3A5033%2F");
    assert_eq!(percent_encode("a&b=c|d,e"), "a%26b%3Dc%7Cd%2Ce");
    assert_eq!(percent_encode("%"), "%25");
    assert_eq!(percent_encode("TRADE:ABC123"), "TRADE%3AABC123");
}

#[test]
fn a_multibyte_character_is_one_escape_per_utf8_byte() {
    assert_eq!(percent_encode("é"), "%C3%A9");
    assert_eq!(percent_encode("€"), "%E2%82%AC");
    assert_eq!(percent_encode("a😀"), "a%F0%9F%98%80");
}

#[test]
fn control_and_high_bytes_are_escaped() {
    assert_eq!(percent_encode("\0\n\x7f"), "%00%0A%7F");
}
