//! The group's one way to read an execution client's ingest lane without blocking: the leverage
//! universe, restore, instant-fill and post-only smokes hold their lane as a [`Pull`] and wait on it
//! here.

use std::time::{Duration, Instant};

use vike_bridge_core::smoke::lifecycle_word;
use vike_exec::Ingest;
use vike_model::events::Event;

/// A non-blocking pull on one client's event lane (the lane's receiver type is the core's runtime
/// type, which this crate does not name).
pub type Pull = Box<dyn FnMut() -> Option<Ingest>>;

/// Every event currently waiting on `rx`.
pub fn drain(rx: &mut Pull) -> Vec<Event> {
    let mut out = Vec::new();
    while let Some(ing) = rx() {
        if let Ingest::Event(e) = ing {
            out.push(e);
        }
    }
    out
}

/// Collect `rx`'s events until `done` accepts the accumulated list or `within` passes.
pub fn wait_for_events(
    rx: &mut Pull,
    within: Duration,
    what: &str,
    done: impl Fn(&[Event]) -> bool,
) -> Vec<Event> {
    let deadline = Instant::now() + within;
    let mut all = Vec::new();
    loop {
        all.extend(drain(rx));
        if done(&all) {
            return all;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}; saw {all:?}");
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Read the lane into `seen` until `done` accepts everything seen, or `secs` pass; every lifecycle
/// event is printed once, with its arrival time since `started` when the caller gives one.
pub fn pump_until(
    pull: &mut Pull,
    seen: &mut Vec<Event>,
    started: Option<Instant>,
    secs: u64,
    done: impl Fn(&[Event]) -> bool,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        while let Some(ingest) = pull() {
            if let Ingest::Event(e) = ingest {
                if lifecycle_word(&e).is_some() {
                    let text = format!("{e:?}").chars().take(300).collect::<String>();
                    match started {
                        Some(t) => println!("EVENT +{:>6}ms {text}", t.elapsed().as_millis()),
                        None => println!("EVENT {text}"),
                    }
                }
                seen.push(e);
            }
        }
        if done(seen) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The lifecycle words `seen` holds for `coid`, in arrival order.
pub fn words(seen: &[Event], coid: &str) -> Vec<&'static str> {
    seen.iter().filter_map(lifecycle_word).filter(|(c, _)| *c == coid).map(|(_, k)| k).collect()
}
