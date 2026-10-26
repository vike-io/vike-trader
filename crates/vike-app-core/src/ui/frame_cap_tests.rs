use super::*;

const STEP: Duration = Duration::from_millis(10);

#[test]
fn the_first_frame_never_waits() {
    let mut cap = FrameCap::new(STEP);
    assert_eq!(cap.pace(Instant::now()), Duration::ZERO);
}

#[test]
fn a_frame_right_after_the_last_waits_the_whole_interval() {
    let mut cap = FrameCap::new(STEP);
    let t = Instant::now();
    cap.pace(t);
    assert_eq!(cap.pace(t), STEP);
}

#[test]
fn a_frame_part_way_through_the_interval_waits_for_the_rest() {
    let mut cap = FrameCap::new(STEP);
    let t = Instant::now();
    cap.pace(t);
    assert_eq!(cap.pace(t + Duration::from_millis(3)), Duration::from_millis(7));
}

/// An idle app's frames are far apart and are never delayed — the cap only bites on a repaint loop —
/// and the next interval is measured from the frame that was NOT delayed.
#[test]
fn a_frame_a_whole_interval_later_is_not_delayed_and_restarts_the_clock() {
    let mut cap = FrameCap::new(STEP);
    let t = Instant::now();
    cap.pace(t);
    assert_eq!(cap.pace(t + STEP), Duration::ZERO, "exactly one interval later");
    assert_eq!(cap.pace(t + Duration::from_secs(5)), Duration::ZERO, "long idle");
    let later = t + Duration::from_secs(5);
    assert_eq!(cap.pace(later + Duration::from_millis(4)), Duration::from_millis(6));
}

/// A loop that asks for a frame the instant the last one ended runs at exactly the cap: after N
/// frames, N-1 intervals have elapsed. This is the property that stops the spin.
#[test]
fn a_repaint_loop_is_held_to_the_cap_rate() {
    let mut cap = FrameCap::new(STEP);
    let start = Instant::now();
    let mut now = start;
    for _ in 0..100 {
        now += cap.pace(now);
    }
    assert_eq!(now - start, STEP * 99);
}

#[test]
fn the_default_cap_is_144_frames_a_second() {
    assert_eq!(max_frames_per_second(), 144);
    let mut cap = FrameCap::default();
    let t = Instant::now();
    cap.pace(t);
    assert_eq!(cap.pace(t), MIN_FRAME_INTERVAL);
}

/// The real clock: two back-to-back `throttle` calls cannot complete in less than the interval,
/// however fast the machine. (A lower bound only; nothing here asserts how long the sleep took.)
#[test]
fn throttle_sleeps_at_least_the_interval_between_two_frames() {
    let mut cap = FrameCap::new(Duration::from_millis(5));
    let start = Instant::now();
    cap.throttle();
    cap.throttle();
    assert!(start.elapsed() >= Duration::from_millis(5), "{:?}", start.elapsed());
}
