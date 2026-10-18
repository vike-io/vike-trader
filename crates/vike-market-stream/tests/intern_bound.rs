//! The `Unsupported` interning set is BOUNDED: a daemon (or a bug) that sends a new reason every time
//! cannot grow the leak. This is its own test binary, with ONE test, because the set is process-wide:
//! the counts below are only meaningful with no other test touching it, and the order of the phases
//! inside the test is the point (the byte cut is observed while the set still has room; once the set
//! is full every new text is the overflow text whatever its length, so a cut checked then proves
//! nothing).

use vike_market_stream::intern::{
    MAX_INTERNED, MAX_INTERNED_BYTES, OVERFLOW_REASON, interned_count, unsupported_reason,
};

#[test]
fn the_leak_is_bounded_and_an_overflowing_reason_is_still_an_honest_unsupported() {
    assert_eq!(interned_count(), 0, "this binary's one test starts with an empty set");

    // PHASE 1, set empty: a reason over the byte bound is CUT (not refused, not the overflow text)
    // and the cut copy is what is interned. With the set full this branch is unreachable, which is why
    // it comes first.
    let long = "x".repeat(MAX_INTERNED_BYTES * 3);
    let cut = unsupported_reason(&long);
    assert_ne!(cut, OVERFLOW_REASON, "a long reason is cut, not refused, while there is room");
    assert_eq!(cut.len(), MAX_INTERNED_BYTES, "cut to exactly the byte bound");
    assert!(long.starts_with(cut), "the interned text is a prefix of the reason");
    assert_eq!(interned_count(), 1, "the cut copy took one place");
    assert!(std::ptr::eq(cut, unsupported_reason(&long)), "and the same text resolves to it again");

    // PHASE 2: fill the rest of the set and run past it.
    let mut got = Vec::new();
    for i in 0..MAX_INTERNED + 25 {
        got.push(unsupported_reason(&format!("bridge reason number {i}")));
    }
    assert_eq!(interned_count(), MAX_INTERNED, "the set stops growing at its bound");
    assert_eq!(got[0], "bridge reason number 0", "the early reasons are kept verbatim");
    // One place went to the cut reason, so the last verbatim one is number MAX_INTERNED - 2.
    assert_eq!(got[MAX_INTERNED - 2], format!("bridge reason number {}", MAX_INTERNED - 2));
    assert_eq!(got[MAX_INTERNED - 1], OVERFLOW_REASON, "the first one past the bound");
    assert_eq!(*got.last().unwrap(), OVERFLOW_REASON, "past the bound the text is the fixed one");
    assert!(OVERFLOW_REASON.contains("unsupported"), "it still says what happened");

    // An already-interned reason keeps resolving once the set is full, to the same pointer.
    let again = unsupported_reason("bridge reason number 3");
    assert!(std::ptr::eq(again, got[3]));
    assert!(std::ptr::eq(cut, unsupported_reason(&long)), "the cut copy too");
}
