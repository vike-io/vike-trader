use super::*;
use std::sync::Mutex;
use std::time::Duration;

/// A network-free feed body: polls its stop flag on the real feeds' cadence.
fn polling_body(stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn spawn_returns_distinct_ids() {
    let mut reg = FeedRegistry::new();
    let id1 = reg.spawn("feed-a".into(), polling_body).expect("spawn ok");
    let id2 = reg.spawn("feed-b".into(), polling_body).expect("spawn ok");
    assert_ne!(id1, id2, "distinct ids per spawn");
    assert_eq!(reg.len(), 2);
    reg.shutdown();
}

/// `raise_stops` must RAISE and JOIN NOTHING — it is phase one of a cross-client teardown, and a
/// version that joined would reintroduce exactly the per-socket serialization it exists to
/// remove. Proven by shape rather than by timing: the registry still holds every subscription
/// afterwards (nothing was drained), while each thread has observed its flag.
#[test]
fn raise_stops_raises_every_flag_without_joining_anything() {
    let mut reg = FeedRegistry::new();
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for i in 0..4 {
        let seen = Arc::clone(&seen);
        reg.spawn(format!("feed-{i}"), move |stop: Arc<AtomicBool>| {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
            seen.fetch_add(1, Ordering::Relaxed);
        })
        .expect("spawn ok");
    }

    reg.raise_stops();
    assert_eq!(reg.len(), 4, "raise_stops must not drain the registry — it joins nothing");

    // Every thread now winds down CONCURRENTLY; the join below is what collects them.
    reg.shutdown();
    assert!(reg.is_empty());
    assert_eq!(seen.load(Ordering::Relaxed), 4, "every thread must have seen its raised flag");
}

#[test]
fn stop_join_stops_only_that_one_feed() {
    let mut reg = FeedRegistry::new();
    let id1 = reg.spawn("feed-a".into(), polling_body).expect("spawn ok");
    let id2 = reg.spawn("feed-b".into(), polling_body).expect("spawn ok");
    reg.stop_join(id1);
    assert_eq!(reg.len(), 1, "only the unsubscribed stream is removed");
    assert!(reg.contains(id2), "the other subscription keeps running");
    assert!(!reg.contains(id1));
    // unknown/already-stopped id is a no-op
    reg.stop_join(id1);
    reg.shutdown();
    assert!(reg.is_empty());
}

#[test]
fn shutdown_joins_every_feed() {
    let mut reg = FeedRegistry::new();
    reg.spawn("feed-a".into(), polling_body).expect("spawn ok");
    reg.spawn("feed-b".into(), polling_body).expect("spawn ok");
    reg.shutdown();
    assert!(reg.is_empty());
}

/// The spawn hook runs ON the spawned thread, BEFORE the body — the affinity-pin seam. Proven
/// by ordering (hook entry precedes body entry in the shared log) and by thread identity (the
/// hook records the spawned thread's name, not the caller's).
#[test]
fn the_spawn_hook_runs_on_the_feed_thread_before_the_body() {
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let hook_log = Arc::clone(&log);
    let mut reg = FeedRegistry::with_spawn_hook(move || {
        let name = std::thread::current().name().unwrap_or("?").to_string();
        hook_log.lock().unwrap().push(format!("hook on {name}"));
    });
    let body_log = Arc::clone(&log);
    reg.spawn("feed-pinned".into(), move |stop| {
        body_log.lock().unwrap().push("body".into());
        polling_body(stop);
    })
    .expect("spawn ok");
    reg.shutdown();
    let log = log.lock().unwrap();
    assert_eq!(
        log.as_slice(),
        ["hook on feed-pinned", "body"],
        "hook first, on the spawned (named) thread, then the body"
    );
}
