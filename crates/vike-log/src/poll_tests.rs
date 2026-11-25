use std::time::{Duration, Instant};

use super::{wait_for, wait_until};

#[test]
fn wait_until_answers_whether_the_condition_held_before_the_deadline() {
    assert!(wait_until(Duration::ZERO, || true), "a condition that holds at once is asked once");

    let start = Instant::now();
    assert!(!wait_until(Duration::from_millis(30), || false), "a condition that never holds");
    assert!(start.elapsed() >= Duration::from_millis(30), "false only once the deadline passed");

    let mut asked = 0;
    assert!(
        wait_until(Duration::from_secs(5), || {
            asked += 1;
            asked == 3
        }),
        "a condition that comes true on the third look"
    );
    assert_eq!(asked, 3, "asking stops once the condition held");
}

#[test]
fn wait_for_returns_once_the_condition_holds_and_panics_naming_what_never_did() {
    let mut asked = 0;
    wait_for(Duration::from_secs(5), "the third look", || {
        asked += 1;
        asked == 3
    });
    assert_eq!(asked, 3);
    let failed = std::panic::catch_unwind(|| wait_for(Duration::ZERO, "a reply", || false));
    let payload = failed.expect_err("a condition that never holds must panic");
    let text = payload.downcast_ref::<String>().map(String::as_str).unwrap_or_default();
    assert_eq!(text, "timed out waiting for a reply");
}
