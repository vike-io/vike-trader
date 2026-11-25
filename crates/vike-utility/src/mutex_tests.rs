use super::*;

/// A mutex poisoned by a thread that panicked holding it still locks, and the value it guards is read
/// and written as the panicking thread left it. Red on a `lock` that unwraps the poison.
#[test]
fn a_poisoned_mutex_still_locks_and_keeps_its_value() {
    let m = Mutex::new(1);
    let poisoned = std::thread::scope(|s| {
        s.spawn(|| {
            let mut held = m.lock().unwrap();
            *held = 2;
            panic!("a scripted crash while holding the lock");
        })
        .join()
    });
    assert!(poisoned.is_err() && m.is_poisoned());
    assert_eq!(*lock(&m), 2, "the value the dying holder wrote");
    *lock(&m) = 3;
    assert_eq!(*lock(&m), 3);
}
