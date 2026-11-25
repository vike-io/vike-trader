//! The pump clock: a thread that entered no virtual clock reads the wall clock; members of one interleave by virtual
//! time and never sleep for real; a timed channel read sees a message sent by another member at its virtual time.

use std::sync::mpsc::channel;

use super::*;

/// No seat: the wall clock, as every shipped thread reads it.
#[test]
fn a_thread_on_no_virtual_clock_reads_the_wall_clock() {
    let before = Instant::now();
    sleep(Duration::from_millis(20));
    assert!(since(before) >= Duration::from_millis(20));
}

/// Two members: an hour of virtual sleeps costs no wall time, and each wakes at its own virtual time.
#[test]
fn members_interleave_by_virtual_time_and_never_sleep_for_real() {
    let clock = VirtualClock::new(2);
    let started = Instant::now();
    let order = Arc::new(Mutex::new(Vec::new()));
    let sleeper = |span: u64, name: &'static str| {
        let (clock, order) = (Arc::clone(&clock), Arc::clone(&order));
        std::thread::spawn(move || {
            let _member = clock.enter();
            for _ in 0..3 {
                sleep(Duration::from_secs(span));
                order.lock().unwrap().push((clock.offset(now()).as_secs(), name));
            }
        })
    };
    let (a, b) = (sleeper(1200, "a"), sleeper(1000, "b"));
    a.join().unwrap();
    b.join().unwrap();
    let mut seen = order.lock().unwrap().clone();
    seen.sort_unstable();
    assert_eq!(
        seen,
        vec![(1000, "b"), (1200, "a"), (2000, "b"), (2400, "a"), (3000, "b"), (3600, "a")]
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "virtual hours took {:?}",
        started.elapsed()
    );
}

/// A timed read on a virtual clock returns the message another member sent, at the sender's virtual time, and times
/// out at its own deadline when nothing comes.
#[test]
fn a_timed_read_sees_a_message_at_its_virtual_time() {
    let clock = VirtualClock::new(2);
    let (tx, rx) = channel::<u32>();
    let sender = {
        let clock = Arc::clone(&clock);
        std::thread::spawn(move || {
            let _member = clock.enter();
            sleep(Duration::from_secs(3));
            tx.send(7).unwrap();
            sleep(Duration::from_secs(60));
        })
    };
    let member = clock.enter();
    assert_eq!(recv_timeout(&rx, Duration::from_secs(10)), Ok(7));
    let got_at = clock.offset(now());
    assert!(got_at >= Duration::from_secs(3) && got_at <= Duration::from_secs(3) + RECV_SLICE);
    assert_eq!(recv_timeout(&rx, Duration::from_secs(5)), Err(RecvTimeoutError::Timeout));
    assert_eq!(clock.offset(now()), got_at + Duration::from_secs(5));
    // Leaving lets the sender's long sleep run out alone.
    drop(member);
    sender.join().unwrap();
}
