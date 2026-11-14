//! The mark itself, over a descriptor whose close-on-exec flag a test cleared (the shape a parent leaves
//! behind). The child-side proof, a strategy process starting with an inherited descriptor only after the
//! mark, is `crates/vike-sandbox/tests/strategy_process_policy.rs`'s.

use super::*;

#[cfg(target_os = "linux")]
#[test]
fn an_inherited_descriptor_is_marked_and_one_already_marked_is_not_counted() {
    use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};
    let file = tempfile::tempfile().expect("a scratch file");
    fcntl_setfd(&file, FdFlags::empty()).expect("clear close-on-exec, as a parent's leak would");
    assert!(!fcntl_getfd(&file).unwrap().contains(FdFlags::CLOEXEC));

    assert!(mark_inherited_close_on_exec() >= 1, "the cleared descriptor is counted");
    assert!(fcntl_getfd(&file).unwrap().contains(FdFlags::CLOEXEC), "and marked, not closed");
    // Still open: the mark never closes.
    assert!(file.metadata().is_ok());
    // A second pass finds this one already marked. (Other tests' descriptors are std's, so the pass is
    // not asserted to find none at all.)
    let _ = mark_inherited_close_on_exec();
    assert!(fcntl_getfd(&file).unwrap().contains(FdFlags::CLOEXEC));
}

#[test]
fn the_line_states_the_count_and_is_silent_when_nothing_was_inherited() {
    assert_eq!(marked_line(0), None);
    let line = marked_line(2).expect("two marked");
    assert!(line.contains("2 above stderr marked close-on-exec"), "{line}");
    assert!(!line.contains('/'), "no path: {line}");
}
