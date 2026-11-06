//! **A panic on a worker thread reaches the log FILE, with the thread's name.** Without a hook a
//! panic prints to stderr only (`thread '<unnamed>' panicked ...`), so the file an operator reads
//! after the fact never mentions it. `init_with_reload` installs the hook; this drives the real path:
//! `init`, a named thread that panics, the rolled JSON file read back.
//!
//! The hook also hands the panic to the hook that was installed BEFORE `init` (the previous hook), so
//! stderr is unchanged: a probe hook installed first must still fire.
//!
//! ⚠ Its own test binary with exactly one test, deliberately: `init` sets the process's GLOBAL
//! subscriber and panic hook, each once, so a second `init` in the same binary would assert nothing.

use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};

static PREVIOUS_HOOK_RAN: AtomicBool = AtomicBool::new(false);

#[test]
fn a_panic_on_a_named_thread_is_logged_with_its_name_message_and_the_previous_hook_still_runs() {
    // ⚠ A bound `TempDir`, not a pid-keyed path: it is removed on every exit path.
    let tmp = tempfile::tempdir().expect("temp log dir");
    let dir = tmp.path().to_path_buf();

    // The hook that was installed before `init`: the new one must call it.
    std::panic::set_hook(Box::new(|_| PREVIOUS_HOOK_RAN.store(true, Ordering::SeqCst)));

    let guards = vike_log::init(vike_log::LogConfig {
        file_prefix: "panic-test".to_string(),
        dir: Some(dir.clone()),
        ..Default::default()
    });

    let joined = std::thread::Builder::new()
        .name("t-panics".to_string())
        .spawn(|| panic!("boom-marker-4217"))
        .expect("spawn the panicking thread")
        .join();
    assert!(joined.is_err(), "the thread must have panicked");
    drop(guards); // flush the non-blocking writer

    let mut all = String::new();
    for entry in fs::read_dir(&dir).expect("log dir exists") {
        all.push_str(&fs::read_to_string(entry.unwrap().path()).unwrap_or_default());
    }
    for needle in ["thread panicked", "t-panics", "boom-marker-4217", "\"target\":\"panic\""] {
        assert!(all.contains(needle), "the log file lacks `{needle}`; it holds:\n{all}");
    }
    assert!(
        PREVIOUS_HOOK_RAN.load(Ordering::SeqCst),
        "the hook installed before `init` must still run after ours"
    );
}
