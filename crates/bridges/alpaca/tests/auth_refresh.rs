use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use vike_alpaca::TokenSource;

// Deterministic clock + counting fake exchanger: each exchange returns a unique token so we can
// prove WHEN a refresh happened, with expires_in = 900 like the real endpoint.
fn harness() -> (TokenSource, Arc<AtomicU64>, Arc<AtomicU64>) {
    let clock = Arc::new(AtomicU64::new(1_000_000));
    let calls = Arc::new(AtomicU64::new(0));
    let (c2, k2) = (clock.clone(), calls.clone());
    let ts = TokenSource::with_clock_and_exchanger(
        Box::new(move || c2.load(Ordering::SeqCst)),
        Box::new(move |_body: &str| {
            let n = k2.fetch_add(1, Ordering::SeqCst) + 1;
            Ok((format!("tok-{n}"), 900))
        }),
    );
    (ts, clock, calls)
}

#[test]
fn first_call_exchanges_then_caches() {
    let (ts, _clock, calls) = harness();
    assert_eq!(ts.bearer().unwrap(), "tok-1");
    assert_eq!(ts.bearer().unwrap(), "tok-1"); // cached — no 2nd exchange
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn refreshes_within_margin_of_expiry() {
    let (ts, clock, calls) = harness();
    assert_eq!(ts.bearer().unwrap(), "tok-1"); // expires at now+900 = 1_000_900
    clock.store(1_000_000 + 900 - 30, Ordering::SeqCst); // 30s left < 60s margin → refresh
    assert_eq!(ts.bearer().unwrap(), "tok-2");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn no_refresh_comfortably_before_expiry() {
    let (ts, clock, calls) = harness();
    assert_eq!(ts.bearer().unwrap(), "tok-1");
    clock.store(1_000_000 + 100, Ordering::SeqCst); // 800s left → still cached
    assert_eq!(ts.bearer().unwrap(), "tok-1");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn force_refresh_always_re_exchanges() {
    let (ts, _clock, calls) = harness();
    assert_eq!(ts.bearer().unwrap(), "tok-1");
    assert_eq!(ts.force_refresh().unwrap(), "tok-2"); // on-401 path
    assert_eq!(ts.bearer().unwrap(), "tok-2"); // new token now cached
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
