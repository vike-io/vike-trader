use super::*;
use crate::rule::AlertTargets;

fn alert(webhooks: &[&str], in_process: bool) -> FiredAlert {
    FiredAlert {
        rule_id: "r1".into(),
        title: "BTC breakout".into(),
        body: "binance BTCUSDT crossed above 100000".into(),
        ts_ms: 42,
        targets: AlertTargets {
            in_process,
            webhooks: webhooks.iter().map(|s| s.to_string()).collect(),
        },
    }
}

/// **[`WEBHOOK_KEYS`] and [`webhook_configs_from_env`] must not drift apart**, because a
/// SCOPED credential reader declares that constant and the drift's symptom would be alerting
/// silently delivering nowhere on a daemon that scoped its read
/// (`crates/vike-datahub/src/recorder.rs`).
///
/// Both directions, as far as they can be checked from a fixed-name reader:
///
/// * **Nothing else is needed** — a map holding ONLY these three names produces the MAXIMAL
///   result (both targets). A fourth required name added to the reader without joining the
///   constant would stop one of those targets materialising, and this reddens.
/// * **Every declared name is USED** — dropping any one of the three from an otherwise complete
///   map changes the result. A name left in the constant after its read was deleted reddens
///   here rather than quietly widening what a scoped process materialises.
#[test]
fn the_declared_webhook_keys_are_the_ones_the_reader_uses() {
    let target_names = |env: &HashMap<String, String>| -> Vec<String> {
        webhook_configs_from_env(env).into_iter().map(|c| c.name).collect()
    };
    let full: HashMap<String, String> =
        WEBHOOK_KEYS.iter().map(|k| ((*k).to_string(), format!("v-{k}"))).collect();
    let complete = target_names(&full);
    assert_eq!(
        complete,
        vec!["telegram".to_string(), "webhook".to_string()],
        "the three declared names alone must build every target this reader can build — a \
             fourth name the reader needs has appeared and is NOT in WEBHOOK_KEYS"
    );
    for dropped in WEBHOOK_KEYS {
        let mut partial = full.clone();
        partial.remove(dropped);
        assert_ne!(
            target_names(&partial),
            complete,
            "{dropped} is declared in WEBHOOK_KEYS but removing it changes nothing — the \
                 reader no longer uses it, so a scoped process is materialising a credential for \
                 no one"
        );
    }
}

/// A transport double that records every POST (never touches the network) and can be told to
/// fail, to exercise the swallow-the-error path.
#[derive(Clone, Default)]
struct FakeTransport {
    calls: Arc<Mutex<Vec<(String, String)>>>,
    fail: bool,
}

impl WebhookTransport for FakeTransport {
    fn post_json(&self, url: &str, body: &str) -> Result<(), String> {
        self.calls.lock().unwrap().push((url.to_string(), body.to_string()));
        if self.fail { Err("boom".to_string()) } else { Ok(()) }
    }
}

#[test]
fn in_process_sink_buffers_only_in_process_targeted_alerts() {
    let sink = InProcessSink::new();
    sink.deliver(&alert(&[], true)); // in-process on → buffered
    sink.deliver(&alert(&["telegram"], false)); // in-process off → NOT buffered
    assert_eq!(sink.len(), 1);
    let drained = sink.drain();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].title, "BTC breakout");
    assert!(sink.is_empty(), "drain clears the buffer");
}

#[test]
fn in_process_handle_shares_the_same_inbox() {
    let sink = InProcessSink::new();
    let handle = sink.handle();
    sink.deliver(&alert(&[], true));
    assert_eq!(handle.lock().unwrap().len(), 1, "the GUI-side handle sees the engine's writes");
}

#[test]
fn webhook_sink_posts_a_telegram_message_for_a_matching_target_only() {
    let fake = FakeTransport::default();
    let sink = WebhookSink::new(
        WebhookConfig {
            name: "telegram".into(),
            kind: WebhookKind::Telegram { token: "123:ABC".into(), chat_id: "9001".into() },
        },
        fake.clone(),
    );
    // routed to "telegram" → one POST.
    sink.deliver(&alert(&["telegram"], true));
    // NOT routed to "telegram" (different name) → no POST.
    sink.deliver(&alert(&["webhook"], true));
    // in-process-only alert → no POST.
    sink.deliver(&alert(&[], true));

    let calls = fake.calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "exactly the one matching-target alert POSTs");
    let (url, body) = &calls[0];
    assert_eq!(url, "https://api.telegram.org/bot123:ABC/sendMessage");
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["chat_id"], "9001");
    assert!(v["text"].as_str().unwrap().contains("BTC breakout"));
    assert!(v["text"].as_str().unwrap().contains("crossed above 100000"));
}

#[test]
fn webhook_sink_posts_a_generic_json_body() {
    let fake = FakeTransport::default();
    let sink = WebhookSink::new(
        WebhookConfig {
            name: "webhook".into(),
            kind: WebhookKind::Generic { url: "https://example.test/hook/secret".into() },
        },
        fake.clone(),
    );
    sink.deliver(&alert(&["webhook"], false));
    let calls = fake.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let (url, body) = &calls[0];
    assert_eq!(url, "https://example.test/hook/secret");
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["rule"], "r1");
    assert_eq!(v["title"], "BTC breakout");
}

#[test]
fn webhook_delivery_failure_is_swallowed_not_propagated() {
    // A failing transport must not panic or propagate — deliver returns normally (the error is
    // logged with the target name + coarse category, never the url).
    let fake = FakeTransport { calls: Arc::new(Mutex::new(Vec::new())), fail: true };
    let sink = WebhookSink::new(
        WebhookConfig {
            name: "telegram".into(),
            kind: WebhookKind::Telegram { token: "t".into(), chat_id: "c".into() },
        },
        fake.clone(),
    );
    sink.deliver(&alert(&["telegram"], true)); // must not panic
    assert_eq!(fake.calls.lock().unwrap().len(), 1, "the POST was attempted");
}

#[test]
fn webhook_config_debug_redacts_the_secret() {
    let tg = WebhookConfig {
        name: "telegram".into(),
        kind: WebhookKind::Telegram {
            token: "123456:SUPER-SECRET-TOKEN".into(),
            chat_id: "9001".into(),
        },
    };
    let dbg = format!("{tg:?}");
    assert!(!dbg.contains("SUPER-SECRET-TOKEN"), "token must never reach Debug: {dbg}");
    assert!(!dbg.contains("123456:SUPER-SECRET-TOKEN"));
    assert!(dbg.contains("<redacted>"));
    assert!(dbg.contains("9001"), "the non-secret chat_id may show for diagnostics");

    let generic = WebhookConfig {
        name: "webhook".into(),
        kind: WebhookKind::Generic {
            url: "https://discord.com/api/webhooks/111/AAA-secret-BBB".into(),
        },
    };
    let dbg = format!("{generic:?}");
    assert!(!dbg.contains("secret"), "a webhook URL is a secret and must not reach Debug: {dbg}");
    assert!(!dbg.contains("discord.com"));
}

#[test]
fn webhook_configs_from_env_builds_only_present_targets() {
    // empty env → no targets (byte-identical to alerting off).
    assert!(webhook_configs_from_env(&HashMap::new()).is_empty());

    // telegram needs BOTH token and chat_id — token alone yields nothing.
    let mut partial = HashMap::new();
    partial.insert("VIKE_ALERT_TELEGRAM_TOKEN".to_string(), "t".to_string());
    assert!(
        webhook_configs_from_env(&partial).is_empty(),
        "an incomplete telegram config builds no target"
    );

    let mut full = HashMap::new();
    full.insert("VIKE_ALERT_TELEGRAM_TOKEN".to_string(), "123:ABC".to_string());
    full.insert("VIKE_ALERT_TELEGRAM_CHAT_ID".to_string(), "9001".to_string());
    full.insert("VIKE_ALERT_WEBHOOK_URL".to_string(), "https://example.test/h".to_string());
    let cfgs = webhook_configs_from_env(&full);
    assert_eq!(cfgs.len(), 2);
    assert_eq!(cfgs[0].name, "telegram");
    assert!(matches!(cfgs[0].kind, WebhookKind::Telegram { .. }));
    assert_eq!(cfgs[1].name, "webhook");
    assert!(matches!(cfgs[1].kind, WebhookKind::Generic { .. }));
    // and even the built config never leaks its token through Debug.
    assert!(!format!("{:?}", cfgs[0]).contains("ABC"));
}

// ── [`QueuedSink`] — the BOUND on the delivery seam ────────────────────────────────────────
//
// These run in the roster lane like everything else here: no vike crate, no network, no clock
// beyond `Instant`. What they pin is the property the type exists for — the CALLER's cost is
// bounded whatever the endpoint does — plus the two things that make a bound honest: the overflow
// is counted, and the shutdown is bounded too.

/// An endpoint that answers SLOWLY. The real one is bounded only by `UreqTransport`'s 10 s.
struct SleepySink {
    delay: Duration,
    seen: Arc<AtomicU64>,
}

impl AlertSink for SleepySink {
    fn deliver(&self, _alert: &FiredAlert) {
        std::thread::sleep(self.delay);
        self.seen.fetch_add(1, Ordering::Relaxed);
    }
}

/// An endpoint that does not answer at ALL until released — a black-holed webhook, the shape
/// that parked the recorder's tick loop. The hard cap is so a failing test cannot leave a
/// thread wedged for the life of the test binary.
struct WedgedSink {
    entered: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
}

impl AlertSink for WedgedSink {
    fn deliver(&self, _alert: &FiredAlert) {
        self.entered.store(true, Ordering::Relaxed);
        let cap = Instant::now() + Duration::from_secs(30);
        while !self.release.load(Ordering::Relaxed) && Instant::now() < cap {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// Poll `cond` until it holds or `budget` runs out. Returns whether it held — every caller
/// asserts on that rather than sleeping a guessed interval, which is how a timing test earns
/// the right to exist on a loaded CI box.
fn wait_until(cond: impl Fn() -> bool, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    cond()
}

/// **The deliverable.** Six alerts into a sink that takes 500 ms each — 3 s of endpoint work —
/// and the caller is back in single-digit milliseconds. Unwrapped, this is the loop the
/// recorder's stop flag sat behind.
///
/// The ceiling is ONE delivery (500 ms): ~50x what six `try_send`s cost on a quiet box (well
/// under 10 ms), so a scheduler stall of a few hundred milliseconds on a loaded CI lane passes,
/// while a caller parked for even a single delivery — let alone the six a synchronous
/// `dispatch` would cost — fails. The count assertion below is the other half and is not
/// timing-sensitive at all.
#[test]
fn a_slow_sink_never_parks_the_caller() {
    const DELAY: Duration = Duration::from_millis(500);
    let seen = Arc::new(AtomicU64::new(0));
    let (sink, stop) =
        QueuedSink::spawn("slow", 16, Box::new(SleepySink { delay: DELAY, seen: seen.clone() }));

    let began = Instant::now();
    for _ in 0..6 {
        sink.deliver(&alert(&["slow"], true));
    }
    let dispatched = began.elapsed();
    assert!(
        dispatched < DELAY,
        "six dispatches took {dispatched:?} — the caller waited on the endpoint for at least \
             one whole delivery ({DELAY:?} each, 3 s in all), which is the whole defect"
    );

    // …and nothing VANISHED: everything is either delivered or counted as abandoned at stop.
    // The budget covers the 3 s drain with room for a loaded box: an `Abandoned` here would be
    // the box, not the type.
    let outcome = stop.shutdown(Duration::from_secs(10));
    assert!(matches!(outcome, QueuedSinkOutcome::Joined { .. }), "{outcome:?}");
    assert_eq!(
        outcome.delivered() + outcome.dropped(),
        6,
        "every alert is accounted for, delivered or dropped: {outcome:?}"
    );
}

/// A wedged endpoint fills the queue, and past it alerts are DROPPED — bounded memory, a
/// caller that still returns, and a count an operator can read. The one thing that must never
/// happen is a silent loss, so the count is the assertion.
///
/// The wall-clock ceiling on the ten dispatches is 2 s — >100x their quiet-box cost — because
/// the regression it guards is a `send` where a `try_send` belongs, and that parks the caller
/// until the wedged sink's own 30 s cap: any ceiling under that catches it, so the ceiling is
/// sized for a loaded lane rather than for precision.
#[test]
fn an_overflowing_queue_drops_and_counts_rather_than_blocking() {
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let (sink, stop) = QueuedSink::spawn(
        "wedged",
        2,
        Box::new(WedgedSink { entered: entered.clone(), release: release.clone() }),
    );

    let began = Instant::now();
    for _ in 0..10 {
        sink.deliver(&alert(&["wedged"], true));
    }
    let dispatched = began.elapsed();
    assert!(
        dispatched < Duration::from_secs(2),
        "the caller blocked ({dispatched:?}) — a full queue must DROP, never wait"
    );
    // At most 2 in the queue plus at most 1 in the worker's hand ⇒ 7 or 8 dropped, depending on
    // whether the worker had taken one yet. Both are correct; a range is the honest assertion.
    let dropped = sink.dropped();
    assert!(
        (7..=8).contains(&dropped),
        "expected the overflow past capacity 2 to be dropped and counted, got {dropped}"
    );

    release.store(true, Ordering::Relaxed);
    let outcome = stop.shutdown(Duration::from_secs(5));
    assert_eq!(
        outcome.delivered() + outcome.dropped(),
        10,
        "delivered + dropped must still account for every alert: {outcome:?}"
    );
}

/// Routing is consulted BEFORE a queue slot is spent, so a target that is not named cannot have
/// its queue filled by alerts meant for another one — the failure this prevents is the ONE
/// alert routed here being the one dropped.
#[test]
fn a_queue_slot_is_never_spent_on_an_alert_this_target_ignores() {
    let fake = FakeTransport::default();
    let hook = WebhookSink::new(
        WebhookConfig {
            name: "telegram".into(),
            kind: WebhookKind::Telegram { token: "t".into(), chat_id: "c".into() },
        },
        fake.clone(),
    );
    let (sink, stop) = QueuedSink::spawn("telegram", 1, Box::new(hook));

    for _ in 0..50 {
        sink.deliver(&alert(&["webhook"], false)); // routed to a DIFFERENT target
    }
    sink.deliver(&alert(&["telegram"], false)); // …and the one that is routed here

    assert!(
        wait_until(|| fake.calls.lock().unwrap().len() == 1, Duration::from_secs(5)),
        "the routed alert never reached the transport"
    );
    assert_eq!(sink.dropped(), 0, "50 unrouted alerts must cost no queue slot at all");
    let outcome = stop.shutdown(Duration::from_secs(5));
    assert_eq!(outcome.delivered(), 1, "{outcome:?}");
}

/// The shutdown is BOUNDED, and it says so when it gives up. A worker caught inside a delivery
/// cannot be interrupted, so the alternative — a plain `join()` — would add the endpoint's
/// whole timeout to its owner's teardown, which on the recorder is sized against a systemd
/// `TimeoutStopSec=`.
#[test]
fn shutdown_abandons_a_worker_stuck_in_a_delivery_within_its_budget() {
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let (sink, stop) = QueuedSink::spawn(
        "stuck",
        4,
        Box::new(WedgedSink { entered: entered.clone(), release: release.clone() }),
    );
    sink.deliver(&alert(&["stuck"], true));
    assert!(
        wait_until(|| entered.load(Ordering::Relaxed), Duration::from_secs(5)),
        "the worker never entered the delivery, so this test would prove nothing"
    );

    let began = Instant::now();
    let outcome = stop.shutdown(Duration::from_millis(200));
    let waited = began.elapsed();
    assert!(
        matches!(outcome, QueuedSinkOutcome::Abandoned { mid_delivery: true, .. }),
        "a worker that cannot finish must be reported as abandoned MID-DELIVERY — the page \
             was on the wire — not silently joined and not reported as idle: {outcome:?}"
    );
    assert!(
        waited < Duration::from_secs(2),
        "the wait must be bounded by the budget it was given; it took {waited:?}"
    );
    // Let the abandoned thread end rather than leaving it wedged for the test binary's life.
    release.store(true, Ordering::Relaxed);
}

/// A budget shorter than one `STOP_POLL` abandons an IDLE worker too — it cannot have woken to
/// read the flag — and that outcome must say the worker was idle, not that a page was on the
/// wire. This is the shape an owner stopping several targets against ONE shared deadline
/// produces for every target after the one that ate the deadline, so a wrong answer here is a
/// false "may not have arrived" in a teardown log, which is the disclosure this type exists for.
#[test]
fn an_idle_worker_abandoned_under_a_zero_budget_is_reported_idle_not_mid_delivery() {
    let seen = Arc::new(AtomicU64::new(0));
    let (sink, stop) = QueuedSink::spawn(
        "idle",
        4,
        Box::new(SleepySink { delay: Duration::ZERO, seen: seen.clone() }),
    );
    // Queue empty, worker parked in its poll: the normal state at a stop. The sink stays ALIVE
    // — dropping it would disconnect the channel and wake the worker into an immediate exit,
    // which is the other path, not this one.
    let outcome = stop.shutdown(Duration::ZERO);
    drop(sink);
    match outcome {
        QueuedSinkOutcome::Abandoned { mid_delivery, delivered, dropped, .. } => {
            assert!(!mid_delivery, "an idle worker was reported as inside a delivery");
            assert_eq!((delivered, dropped), (0, 0));
        }
        // A zero budget can only join a worker that had ALREADY exited, which an idle one has
        // not — but a scheduler that ran it first is not a defect, so this arm is tolerated.
        QueuedSinkOutcome::Joined { delivered, dropped, .. } => {
            assert_eq!((delivered, dropped), (0, 0));
        }
    }
}

/// The envelope guarantee: an alert that lands in the queue AFTER the worker has been told to
/// stop — the slot a drain-then-drop would free and then destroy uncounted — is still either
/// delivered or counted as dropped. `delivered + dropped` reaches the number sent, with no
/// budget for the worker and a producer still running the whole time.
#[test]
fn an_alert_landing_across_the_stop_is_delivered_or_counted_never_lost() {
    let seen = Arc::new(AtomicU64::new(0));
    let (sink, stop) = QueuedSink::spawn(
        "late",
        2,
        Box::new(SleepySink { delay: Duration::from_millis(1), seen: seen.clone() }),
    );
    // Raise the stop with NO budget, so the worker is left to notice on its own — that is the
    // window in which a producer can still land alerts in the buffer.
    let _ = stop.shutdown(Duration::ZERO);
    const SENT: u64 = 40;
    for _ in 0..SENT {
        sink.deliver(&alert(&["late"], true));
        std::thread::sleep(Duration::from_millis(3));
    }
    assert!(
        wait_until(
            || seen.load(Ordering::Relaxed) + sink.dropped() == SENT,
            Duration::from_secs(5)
        ),
        "delivered ({}) + dropped ({}) never reached {SENT}: an alert vanished uncounted",
        seen.load(Ordering::Relaxed),
        sink.dropped()
    );
}

/// Capacity 0 would be a RENDEZVOUS channel, where `try_send` succeeds only when the worker is
/// already parked in `recv` — a "queue" that drops almost everything. It is clamped to 1.
#[test]
fn a_zero_capacity_queue_is_clamped_rather_than_becoming_a_rendezvous() {
    let seen = Arc::new(AtomicU64::new(0));
    let (sink, stop) = QueuedSink::spawn(
        "clamped",
        0,
        Box::new(SleepySink { delay: Duration::from_millis(50), seen: seen.clone() }),
    );
    sink.deliver(&alert(&["clamped"], true));
    assert!(
        wait_until(|| seen.load(Ordering::Relaxed) == 1, Duration::from_secs(5)),
        "a capacity-0 queue swallowed the alert instead of holding one"
    );
    let outcome = stop.shutdown(Duration::from_secs(5));
    assert_eq!(outcome.delivered(), 1, "{outcome:?}");
}

/// `queued_ureq_webhook_sinks` pairs its two `Vec`s positionally, and spawns nothing for an
/// empty config list — the unconfigured recorder mounts no delivery thread at all.
#[test]
fn the_queued_constructor_pairs_its_outputs_and_spawns_nothing_when_unconfigured() {
    let (sinks, stops) = queued_ureq_webhook_sinks(Vec::new(), DEFAULT_QUEUE_CAPACITY);
    assert!(sinks.is_empty() && stops.is_empty(), "no targets ⇒ no sinks and no threads");

    let mut full = HashMap::new();
    full.insert("VIKE_ALERT_TELEGRAM_TOKEN".to_string(), "123:ABC".to_string());
    full.insert("VIKE_ALERT_TELEGRAM_CHAT_ID".to_string(), "9001".to_string());
    full.insert("VIKE_ALERT_WEBHOOK_URL".to_string(), "https://example.test/h".to_string());
    let (sinks, stops) =
        queued_ureq_webhook_sinks(webhook_configs_from_env(&full), DEFAULT_QUEUE_CAPACITY);
    assert_eq!(sinks.len(), 2);
    assert_eq!(stops.iter().map(|s| s.name()).collect::<Vec<_>>(), ["telegram", "webhook"]);
    // Nothing was delivered, so nothing touches the network — the threads are idle and join at
    // once.
    for stop in stops {
        let outcome = stop.shutdown(Duration::from_secs(5));
        assert!(matches!(outcome, QueuedSinkOutcome::Joined { .. }), "{outcome:?}");
        assert_eq!(outcome.delivered() + outcome.dropped(), 0);
    }
}
