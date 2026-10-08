//! **`vike_log::capture::captured` sees an event whose callsite ANOTHER thread registered first,
//! while the capture was live** — the `vike-fxcm` mount-test flake, reduced to the one interleaving
//! that caused it and made deterministic.
//!
//! tracing-core caches one process-wide `Interest` per callsite, computed when it is first hit.
//! While a capture's subscriber is the only live dispatcher, that computation asks only the
//! default of the thread that hits the callsite first — so a sibling test running the same code
//! uncaptured registers it against `NoSubscriber`, caches `never`, and the capture's own hit is
//! dropped before any dispatcher sees it. `crates/vike-log/src/capture.rs`'s `InterestFloor`
//! carries the mechanism and the cure. (This file moved here from `vike-bridge-core`'s `tests/`
//! with the capture itself; `crates/vike-log/tests/scoped_sees_every_callsite.rs` is its twin for
//! the other door.)
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
