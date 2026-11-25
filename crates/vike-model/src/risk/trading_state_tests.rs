use super::*;

/// The order the doc states, and that it is a strict total order over the three variants (a tie
/// would let a profile "loosen" a restored state without the comparison noticing).
#[test]
fn strictness_orders_active_below_reducing_below_halted() {
    assert!(TradingState::Active.strictness() < TradingState::Reducing.strictness());
    assert!(TradingState::Reducing.strictness() < TradingState::Halted.strictness());
}

/// The journal and the wire keep the variant name; a run profile's lowercase word reads as the same
/// state, and the profile's absent key is `Active`.
#[test]
fn the_variant_name_is_written_and_both_spellings_read() {
    use TradingState::{Active, Halted, Reducing};
    for (state, word) in [(Active, "Active"), (Reducing, "Reducing"), (Halted, "Halted")] {
        assert_eq!(serde_json::to_string(&state).expect("serializes"), format!("\"{word}\""));
        for spelled in [word.to_string(), word.to_ascii_lowercase()] {
            let read: TradingState =
                serde_json::from_str(&format!("\"{spelled}\"")).expect("a known spelling");
            assert_eq!(read, state, "{spelled}");
        }
    }
    assert_eq!(TradingState::default(), Active);
}
