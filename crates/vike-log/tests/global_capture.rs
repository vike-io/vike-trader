//! **`vike_log::capture::global` records ONE target's events from every thread of the process, and
//! nothing else.** Its own binary: the door installs the process's global default, which a process
//! holds once.

use vike_log::capture::global;

#[test]
fn the_global_capture_records_its_target_from_any_thread_and_nothing_else() {
    assert!(
        global("global_capture::kept").is_empty(),
        "the first call installs and records nothing"
    );
    std::thread::spawn(|| {
        tracing::info!(target: "global_capture::kept", n = 7_u64, flag = true, who = "server", "kept line");
        tracing::info!(target: "global_capture::dropped", "another target");
    })
    .join()
    .expect("the emitting thread ran");
    let events = global("global_capture::kept");
    assert_eq!(events.len(), 1, "{events:?}");
    let e = &events[0];
    assert_eq!(e.target, "global_capture::kept");
    assert_eq!(e.message, "kept line");
    assert_eq!(
        (e.field("n"), e.field("flag"), e.field("who")),
        (Some("7"), Some("true"), Some("server"))
    );
    let other = std::panic::catch_unwind(|| global("global_capture::dropped"));
    assert!(other.is_err(), "a second target is refused, never silently empty");
}
