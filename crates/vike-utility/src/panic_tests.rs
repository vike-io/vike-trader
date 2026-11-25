use super::*;

/// A literal panic, a formatted one and a boxed payload passed whole all read as their text; a
/// payload that is not text reads as the one fallback word. Red on a copy that drops either text
/// shape or reads the box instead of what it holds.
#[test]
fn a_panic_reads_as_its_text_and_anything_else_as_the_one_fallback() {
    // The panics are expected and reported by the default hook on stderr; the text is what is asserted.
    let literal = std::panic::catch_unwind(|| -> u8 { panic!("a literal") }).unwrap_err();
    assert_eq!(payload_text(&*literal), "a literal");

    let n = 7;
    let formatted = std::panic::catch_unwind(|| -> u8 { panic!("formatted {n}") }).unwrap_err();
    assert_eq!(payload_text(&*formatted), "formatted 7");
    assert_eq!(payload_text(&formatted), "formatted 7", "the box itself is opened");

    let other = std::panic::catch_unwind(|| -> u8 { std::panic::panic_any(42_u32) }).unwrap_err();
    assert_eq!(payload_text(&*other), NOT_TEXT);
}
