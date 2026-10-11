//! **`vike_log::capture::scoped` delivers an event whose callsite ANOTHER thread registered first,
//! to a subscriber the CALLER built** — `crates/vike-log/tests/capture_sees_every_callsite.rs`'s
//! twin for the other door.
//!
//! That file proves the recording double. This one proves the floor is the DOOR's property and not
//! the recorder's: the subscriber here is a formatter into a buffer, built here and handed in.
//! `crates/vike-log/src/capture.rs`'s `InterestFloor` carries the mechanism.
//!
//! ⚠ ONE test, in its OWN binary, for the same reason as its twin: the precondition is
//! process-global, and a second test here opening a scope of its own would make an unfloored door
//! pass by accident.

use std::io::Write;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("buffer lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The one callsite under test, so which thread hits it FIRST is a fact the test controls.
fn probe() {
    tracing::warn!(probe = "scoped_interest", "the scoped probe line");
}

#[test]
fn a_callsite_a_bare_thread_registered_first_still_reaches_the_callers_subscriber() {
    let buf = SharedBuf::default();
    let make = {
        let buf = buf.clone();
        move || buf.clone()
    };
    let subscriber = tracing_subscriber::fmt().with_writer(make).with_ansi(false).finish();
    vike_log::capture::scoped(subscriber, || {
        // A thread with NO subscriber hits the callsite first, mid-scope.
        std::thread::spawn(probe).join().expect("the bare thread ran");
        probe();
    });
    let out = String::from_utf8(buf.0.lock().expect("buffer lock").clone()).expect("utf8 output");
    assert_eq!(
        out.matches("the scoped probe line").count(),
        1,
        "the scoped thread's own hit must reach the caller's subscriber, whichever thread \
         registered the callsite first: {out:?}"
    );
}
