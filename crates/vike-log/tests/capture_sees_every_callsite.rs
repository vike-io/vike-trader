//! **`vike_log::capture::captured` sees an event whose callsite ANOTHER thread registered first,
//! while the capture was live**, made deterministic. `crates/vike-log/src/capture.rs`'s
//! `InterestFloor` carries the mechanism and the cure;
//! `crates/vike-log/tests/scoped_sees_every_callsite.rs` is its twin for the other door.
//!
//! ⚠ ONE test, in its OWN binary, deliberately. The precondition is process-global — the capture's
//! recorder must be the only live dispatcher when the bare thread first hits the callsite — and a
//! second test here running a capture of its own would make the unfixed capture pass by accident.
//! Under `cargo test` this binary is its own process; under nextest every test is.

use vike_log::capture::captured;

/// The one callsite under test, so which thread hits it FIRST is a fact the test controls.
fn probe() {
    tracing::warn!(probe = "interest", "the probe line");
}

#[test]
fn a_callsite_a_bare_thread_registered_first_is_still_captured() {
    let ((), events) = captured(|| {
        // A thread with NO subscriber hits the callsite first, mid-capture: the shape of a sibling
        // test calling the same mount without capturing it.
        std::thread::spawn(probe).join().expect("the bare thread ran");
        probe();
    });
    assert_eq!(
        events.len(),
        1,
        "the capturing thread's own hit must be captured, whichever thread registered the \
         callsite first: {events:?}"
    );
    assert_eq!(events[0].field("probe"), Some("interest"), "{events:?}");
}
