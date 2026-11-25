use super::*;

/// An answer in time comes through whole, on a thread carrying the caller's name; a late one is
/// `TimedOut` (the wait did not sit out the call); a call that panics is `Panicked`, not a timeout.
/// Red on a wait that ignores the deadline or reads a crash as slowness.
#[test]
fn an_answer_in_time_comes_through_and_a_late_or_crashed_one_does_not() {
    let named = bounded("utility-bounded", Duration::from_secs(5), || {
        std::thread::current().name().map(str::to_string)
    });
    assert_eq!(named.ok().flatten().as_deref(), Some("utility-bounded"));

    let late = bounded("utility-bounded", Duration::from_millis(50), || {
        std::thread::sleep(Duration::from_millis(800));
        8
    });
    assert!(matches!(late, Err(ThreadNoAnswer::TimedOut)), "{late:?}");

    // The panic is expected and reported by the default hook on stderr; the answer is what is asserted.
    let crashed: Result<u8, _> =
        bounded("utility-bounded", Duration::from_secs(5), || panic!("a scripted crash"));
    assert!(matches!(crashed, Err(ThreadNoAnswer::Panicked)), "{crashed:?}");
}

/// A thread that ends in time is joined with its answer, a panicked one is joined with its panic,
/// and one still running at the limit comes back as its handle, which a later wait can still join.
/// Red on a join that blocks past the limit or reads a panic as a thread left running.
#[test]
fn a_thread_ending_in_time_is_joined_and_a_running_one_is_handed_back() {
    let quick = std::thread::spawn(|| 8);
    assert_eq!(join_within(quick, Duration::from_secs(5)).ok().map(Result::ok), Some(Some(8)));

    // The panic is expected and reported by the default hook on stderr; the join is what is asserted.
    let crashed = std::thread::spawn(|| -> u8 { panic!("a scripted crash") });
    assert!(matches!(join_within(crashed, Duration::from_secs(5)), Ok(Err(_))));

    let (release, wait) = mpsc::channel::<()>();
    let slow = std::thread::spawn(move || wait.recv().is_ok());
    let started = Instant::now();
    let still_running = join_within(slow, Duration::from_millis(50)).expect_err("still running");
    assert!(started.elapsed() < Duration::from_secs(5), "the limit bounds the wait");
    release.send(()).unwrap();
    assert_eq!(
        join_within(still_running, Duration::from_secs(5)).ok().map(Result::ok),
        Some(Some(true))
    );
}
