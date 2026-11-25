use super::RangeReadGate;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Counted, not timed: twelve readers through a gate of three. Each holds its place until its GROUP of
/// three has entered (a ticket count that only grows), so a gate that admitted fewer than three would
/// leave a group waiting at the hang guard, and one that admitted more would show four inside.
#[test]
fn a_gate_holds_exactly_its_cap_and_lets_every_read_through() {
    const CAP: usize = 3;
    let gate = RangeReadGate::leaked(CAP, CAP);
    let counts = Arc::new([AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0)]);
    let readers: Vec<_> = (0..4 * CAP)
        .map(|i| {
            let counts = Arc::clone(&counts);
            std::thread::spawn(move || {
                let [entered, inside, most] = &*counts;
                let _slot = gate.take(&format!("client-{}", i % 2));
                let ticket = entered.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(inside.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                let release = ticket.div_ceil(CAP) * CAP;
                let guard = Instant::now() + Duration::from_secs(20);
                while entered.load(Ordering::SeqCst) < release && Instant::now() < guard {
                    std::thread::yield_now();
                }
                inside.fetch_sub(1, Ordering::SeqCst);
            })
        })
        .collect();
    for reader in readers {
        reader.join().expect("a reader");
    }
    let [entered, _, most] = &*counts;
    assert_eq!(entered.load(Ordering::SeqCst), 4 * CAP, "every read got a place");
    assert_eq!(most.load(Ordering::SeqCst), CAP, "the cap, and no more, inside at once");
    let held = gate.held.lock().unwrap();
    assert_eq!((held.total, held.by_client.len()), (0, 0), "every place was given back");
}

/// One client holding its whole share (three of four places) does not shut another out: the other's read
/// is admitted at once. The first client's fourth read waits until one of its own places comes back.
#[test]
fn one_client_cannot_hold_every_place() {
    let gate = RangeReadGate::leaked(4, 3);
    let mut a: Vec<_> = (0..3).map(|_| gate.take("desktop")).collect();
    let b = gate.take("backtesthub");
    let (tx, rx) = mpsc::channel();
    let fourth = std::thread::spawn(move || {
        let slot = gate.take("desktop");
        tx.send(()).expect("report the place");
        drop(slot);
    });
    assert!(
        rx.recv_timeout(Duration::from_millis(300)).is_err(),
        "a client past its share waits for one of its own places"
    );
    a.pop();
    rx.recv_timeout(Duration::from_secs(20)).expect("its place came back, so it reads");
    fourth.join().expect("the fourth read");
    drop((a, b));
    assert_eq!(gate.held.lock().unwrap().total, 0);
}
