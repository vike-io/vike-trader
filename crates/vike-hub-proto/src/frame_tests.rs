use std::collections::BTreeMap;

use super::json_body_len;

/// The datahub's frame budget and the gatewayhub client's pager both size rows with it: the count is
/// the byte length `serde_json::to_vec` would produce, and a value serde refuses (a map keyed by
/// something that is not a string) is `None`, never a partial count. MUTATION PROOF: count anything
/// but `buf.len()` in `ByteCount::write`, or answer `Some` on an error, and this fails.
#[test]
fn json_body_len_is_the_serialized_length_and_none_for_an_unwritable_value() {
    let row = serde_json::json!({"ts": 1, "px": 1.5, "sym": "BTCUSDT", "levels": [[1, 2], [3, 4]]});
    assert_eq!(json_body_len(&row), Some(serde_json::to_vec(&row).expect("json").len()));
    assert_eq!(json_body_len(&Vec::<u8>::new()), Some(2), "`[]`");
    let unwritable: BTreeMap<Vec<u8>, u8> = BTreeMap::from([(vec![1], 1)]);
    assert_eq!(json_body_len(&unwritable), None, "a non-string map key is refused by serde_json");
}
