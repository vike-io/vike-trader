//! `RetriedWebhook` tests: a failed delivery is asked again, and one that fails every ask is counted and said once.

use super::*;
use crate::delivery::retried::RetriedWebhook;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

/// A transport that fails its first `fail` posts and counts every ask.
struct Flaky {
    fail: usize,
    asked: Arc<AtomicUsize>,
}

impl WebhookTransport for Flaky {
    fn post_json(&self, _url: &str, _body: &str) -> Result<(), WebhookFailure> {
        let asked = self.asked.fetch_add(1, Ordering::Relaxed);
        if asked < self.fail { Err(WebhookFailure::Status(503)) } else { Ok(()) }
    }
}

/// **A failed delivery is asked again, and one that fails every ask is counted and said once**: one failure then a
/// landing costs two asks and counts nothing; a target that never answers is asked three times (the two pauses),
/// counted once and said at `warn` with the total, the target's name and the coarse category, never the url. An
/// alert not routed to the target is never asked.
#[test]
fn a_failing_delivery_is_retried_and_counted() {
    let failed = Arc::new(AtomicU64::new(0));
    let target = |fail: usize, asked: &Arc<AtomicUsize>| {
        RetriedWebhook::new(
            generic_config("https://hooks.invalid/secret-path"),
            Flaky { fail, asked: Arc::clone(asked) },
            &[Duration::ZERO, Duration::ZERO],
            Arc::clone(&failed),
        )
    };
    let routed = alert(&["webhook"], false);

    let asked = Arc::new(AtomicUsize::new(0));
    target(1, &asked).deliver(&routed);
    assert_eq!(asked.load(Ordering::Relaxed), 2, "one failure, then it lands");
    assert_eq!(failed.load(Ordering::Relaxed), 0);

    let asked = Arc::new(AtomicUsize::new(0));
    let ((), events) = vike_log::capture::captured(|| target(usize::MAX, &asked).deliver(&routed));
    assert_eq!(asked.load(Ordering::Relaxed), 3, "three asks, then it gives up");
    assert_eq!(failed.load(Ordering::Relaxed), 1);
    let [line] = events.as_slice() else { panic!("one line for the one loss: {events:?}") };
    assert_eq!(line.level, tracing::Level::WARN);
    assert_eq!(line.field("target_name"), Some("webhook"));
    assert_eq!(line.field("error"), Some("http 503"));
    assert_eq!(line.field("failed_total"), Some("1"));
    // The url is the secret (a Telegram url holds the token): no field carries it.
    assert!(line.fields.iter().all(|(_, v)| v != "https://hooks.invalid/secret-path"));

    let asked = Arc::new(AtomicUsize::new(0));
    target(0, &asked).deliver(&alert(&[], true));
    assert_eq!(asked.load(Ordering::Relaxed), 0, "not routed to it: never asked");
}
