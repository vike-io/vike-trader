use super::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};
use vike_data::live::{LiveDataError, SubscriptionId};

/// `(symbol, family as membership reported it AT subscribe time)` — the ordering evidence.
type SubscribeLog = Rc<RefCell<Vec<(String, Option<String>)>>>;

/// Records, at the moment of each subscribe, what [`Membership`] said about that symbol — which
/// is exactly how the publish-before-subscribe ordering is pinned: the store's resolver reads
/// the same map at flush time, and the first frame can arrive the instant this returns.
struct CountingClient {
    next: u64,
    venue: String,
    membership: Membership,
    /// Shared so a test reads it after the feed is moved into the runtime.
    subscribed: SubscribeLog,
}

impl CountingClient {
    fn issue(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.next += 1;
        let fam = self.membership.family_of(&self.venue, symbol);
        self.subscribed.borrow_mut().push((symbol.to_string(), fam));
        Ok(SubscriptionId(self.next))
    }
}

impl DataClient for CountingClient {
    fn subscribe_bars(&mut self, _s: &str, _i: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("no bars"))
    }
    fn subscribe_quotes(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.issue(s)
    }
    fn subscribe_trades(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.issue(s)
    }
    fn subscribe_book(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.issue(s)
    }
    fn unsubscribe(&mut self, _id: SubscriptionId) {}
    fn shutdown(&mut self) {}
}

/// A scripted feed: `script` is consulted per tick, so a test drives rotation and outages by
/// wall-clock value.
struct ScriptedFeed {
    venue: String,
    family: Option<String>,
    client: CountingClient,
    script: Rc<dyn Fn(i64) -> Result<Vec<&'static str>, String>>,
    /// A stream this venue delivers as a side effect of another, so subscribing it separately
    /// would duplicate rows — the Polymarket `Book`-implies-`Quotes` shape.
    redundant: Option<Stream>,
}

impl VenueFeed for ScriptedFeed {
    fn venue(&self) -> &str {
        &self.venue
    }
    fn family(&self) -> Option<&str> {
        self.family.as_deref()
    }
    fn desired(&mut self, now_ms: i64) -> Result<BTreeSet<String>, String> {
        (self.script)(now_ms).map(|v| v.into_iter().map(String::from).collect())
    }
    fn client(&mut self) -> &mut dyn DataClient {
        &mut self.client
    }
    fn narrow(&self, requested: &[Stream]) -> Vec<Stream> {
        requested.iter().copied().filter(|s| Some(*s) != self.redundant).collect()
    }
}

/// Returns the feed plus the shared `(symbol, family-at-subscribe-time)` log.
fn feed(
    membership: &Membership,
    family: Option<&str>,
    script: impl Fn(i64) -> Result<Vec<&'static str>, String> + 'static,
) -> (Box<ScriptedFeed>, SubscribeLog) {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let f = Box::new(ScriptedFeed {
        venue: "polymarket".into(),
        family: family.map(String::from),
        client: CountingClient {
            next: 0,
            venue: "polymarket".into(),
            membership: membership.clone(),
            subscribed: seen.clone(),
        },
        script: Rc::new(script),
        redundant: None,
    });
    (f, seen)
}

#[test]
fn a_family_is_published_to_membership_before_its_symbols_are_subscribed() {
    let m = Membership::new();
    let (f, seen) = feed(&m, Some("btc-5m"), |_| Ok(vec!["UP", "DOWN"]));
    let mut rt = RecorderRuntime::new(m.clone(), vec![Stream::Book]);
    rt.add_feed(f);

    rt.tick(1_000);

    // The client logged what membership said AT each subscribe. Both tokens were already mapped,
    // so a frame arriving on the very first read commits into the group rather than per-symbol.
    let seen = seen.borrow();
    assert_eq!(seen.len(), 2);
    assert!(
        seen.iter().all(|(_, f)| f.as_deref() == Some("btc-5m")),
        "membership was not published before subscribing: {seen:?}"
    );
}

/// The outage case. An empty desired set and an unresolvable one look the same to a naive loop
/// and mean opposite things: one is "this family has no live market", the other is "I do not
/// know". Reconciling the second would unsubscribe every live book.
#[test]
fn a_resolution_failure_leaves_every_subscription_untouched() {
    let m = Membership::new();
    let (f, _) = feed(&m, Some("btc-5m"), |now| {
        if now < 2_000 { Ok(vec!["UP", "DOWN"]) } else { Err("gamma: connection reset".into()) }
    });
    let mut rt = RecorderRuntime::new(m.clone(), vec![Stream::Book]);
    rt.add_feed(f);

    rt.tick(1_000);
    assert_eq!(rt.subscription_count(), 2);

    let out = rt.tick(3_000);

    assert!(matches!(out[0], FeedTick::ResolveFailed { .. }), "{out:?}");
    assert_eq!(rt.subscription_count(), 2, "the live books survived the outage");
    assert_eq!(
        m.family_of("polymarket", "UP").as_deref(),
        Some("btc-5m"),
        "membership must not be cleared either — the rows still arriving belong to the family"
    );
}

/// The distinction the previous test depends on: a family that genuinely resolves to nothing
/// DOES release its subscriptions.
#[test]
fn a_family_that_resolves_to_nothing_does_unsubscribe() {
    let m = Membership::new();
    let (f, _) =
        feed(
            &m,
            Some("btc-5m"),
            |now| {
                if now < 2_000 { Ok(vec!["UP", "DOWN"]) } else { Ok(vec![]) }
            },
        );
    let mut rt = RecorderRuntime::new(m.clone(), vec![Stream::Book]);
    rt.add_feed(f);

    rt.tick(1_000);
    rt.tick(3_000);

    assert_eq!(rt.subscription_count(), 0);
    assert!(m.is_empty(), "and the membership map is cleared, not left routing dead tokens");
}

#[test]
fn a_rotation_republishes_membership_and_moves_the_subscriptions() {
    let m = Membership::new();
    let (f, _) = feed(&m, Some("btc-5m"), |now| {
        if now < 2_000 { Ok(vec!["UP", "DOWN"]) } else { Ok(vec!["NEW"]) }
    });
    let mut rt = RecorderRuntime::new(m.clone(), vec![Stream::Book]);
    rt.add_feed(f);

    rt.tick(1_000);
    let out = rt.tick(3_000);

    match &out[0] {
        FeedTick::Reconciled { report, symbols, .. } => {
            assert_eq!(*symbols, 1);
            assert_eq!(report.stopped.len(), 2);
            assert_eq!(report.started.len(), 1);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(m.family_of("polymarket", "NEW").as_deref(), Some("btc-5m"));
    // INVERTED (2026-08-02): this asserted `None` for the departed token, and that assertion was
    // the bug, pinned — a rotation must not strand rows still buffered for the old window. See
    // `Membership::set_family`'s retire note. "Moved" is now `len()`: UP is no longer LIVE.
    assert_eq!(
        m.family_of("polymarket", "UP").as_deref(),
        Some("btc-5m"),
        "retired, so its last flush still lands in the family"
    );
    assert_eq!(m.len(), 1, "only NEW is live");
}

/// An explicit symbol list has no family, so nothing is published and its rows stay per-symbol —
/// the `config` module's documented split, enforced here rather than only described.
#[test]
fn an_explicit_symbol_subscription_publishes_no_membership() {
    let m = Membership::new();
    let (f, _) = feed(&m, None, |_| Ok(vec!["BTCUSDT"]));
    let mut rt = RecorderRuntime::new(m.clone(), vec![Stream::Book]);
    rt.add_feed(f);

    rt.tick(1_000);

    assert_eq!(rt.subscription_count(), 1);
    assert!(m.is_empty(), "no family ⇒ no group ⇒ per-symbol series");
}

/// One feed's outage must not stall the others — a Gamma blip cannot stop a Binance recording.
#[test]
fn one_feeds_failure_does_not_block_the_others() {
    let m = Membership::new();
    let (bad, _) = feed(&m, Some("bad"), |_| Err("down".into()));
    let (good, _) = feed(&m, Some("good"), |_| Ok(vec!["UP"]));
    let mut rt = RecorderRuntime::new(m.clone(), vec![Stream::Book]);
    rt.add_feed(bad);
    rt.add_feed(good);

    let out = rt.tick(1_000);

    assert_eq!(out.len(), 2);
    assert!(matches!(out[0], FeedTick::ResolveFailed { .. }));
    assert!(matches!(out[1], FeedTick::Reconciled { .. }));
    assert_eq!(rt.subscription_count(), 1);
}

/// The duplicate-rows guard. On Polymarket `subscribe_book` already emits derived L1 quotes, so
/// a profile asking for quotes AND book must NOT open a second quote pump — it would derive the
/// same L1 from its own copy of the book and write every quote row twice, into the customer's
/// tape, where it later reads as double volume.
#[test]
fn a_venue_can_drop_a_stream_it_already_delivers_via_another() {
    let m = Membership::new();
    let (mut f, seen) = feed(&m, Some("btc-5m"), |_| Ok(vec!["UP"]));
    f.redundant = Some(Stream::Quotes);
    let mut rt = RecorderRuntime::new(m, vec![Stream::Quotes, Stream::Trades, Stream::Book]);
    rt.add_feed(f);

    rt.tick(1_000);

    assert_eq!(rt.subscription_count(), 2, "trades + book, no separate quote pump");
    assert_eq!(seen.borrow().len(), 2);
}

// -- the never-resolved distinction, and the dry-run predicate ---------------------------------

/// **The measured failure, as data.** A proxy nothing is listening behind: every tick fails to
/// resolve, and the feed has never once produced a symbol. `last_nonempty_ms` is `None`, which
/// is what separates this from a mid-run blip — and the daemon ran forever in this state
/// logging one `warn!` a tick while recording nothing.
#[test]
fn a_feed_that_never_resolved_reports_no_last_nonempty() {
    let m = Membership::new();
    let (f, _) = feed(&m, Some("btc-5m"), |_| Err("network: io: Connection refused".into()));
    let mut rt = RecorderRuntime::new(m, vec![Stream::Book]);
    rt.add_feed(f);

    for now in [1_000, 31_000, 61_000] {
        let out = rt.tick(now);
        assert_eq!(out[0].last_nonempty_ms(), None, "never resolved, at t={now}");
        assert_eq!(out[0].live(), 0);
    }
}

/// …and the mirror image, which must NOT escalate: a feed that resolved and then hit an outage
/// carries the timestamp of when it last worked. `runtime`'s module doc and
/// `a_resolution_failure_leaves_every_subscription_untouched` are why that is a retry.
#[test]
fn a_feed_that_resolved_then_failed_keeps_its_last_nonempty() {
    let m = Membership::new();
    let (f, _) = feed(&m, Some("btc-5m"), |now| {
        if now < 2_000 { Ok(vec!["UP", "DOWN"]) } else { Err("gamma: connection reset".into()) }
    });
    let mut rt = RecorderRuntime::new(m, vec![Stream::Book]);
    rt.add_feed(f);

    assert_eq!(rt.tick(1_000)[0].last_nonempty_ms(), Some(1_000));
    let out = rt.tick(3_000);
    assert!(matches!(out[0], FeedTick::ResolveFailed { .. }));
    assert_eq!(out[0].last_nonempty_ms(), Some(1_000), "it worked at t=1_000 — a retry, not a gap");
    assert_eq!(out[0].live(), 2, "…and its books are still live");
}

/// ⚠ **The QUIETER sibling, and the reason the field tracks NON-EMPTY rather than `Ok`.** A
/// family that resolves to zero symbols returns `Ok`, reconciles to nothing, and produces a
/// quiet report the daemon does not even log. Tracking `Ok` would leave it invisible.
#[test]
fn a_family_that_only_ever_resolves_to_nothing_reports_no_last_nonempty() {
    let m = Membership::new();
    let (f, _) = feed(&m, Some("typo-5m"), |_| Ok(vec![]));
    let mut rt = RecorderRuntime::new(m, vec![Stream::Book]);
    rt.add_feed(f);

    let out = rt.tick(1_000);
    assert!(matches!(out[0], FeedTick::Reconciled { .. }), "it is an Ok, not an error: {out:?}");
    assert_eq!(out[0].last_nonempty_ms(), None, "…and it has still never produced a symbol");
    assert_eq!(out[0].live(), 0);
    match &out[0] {
        FeedTick::Reconciled { report, .. } => {
            assert!(report.is_quiet(), "the daemon prints NOTHING for this tick: {report:?}")
        }
        other => panic!("{other:?}"),
    }
}

/// A healthy tick proves the dry run can pass at all — without this the predicate below could
/// be "always fail" and every other assertion would still hold.
#[test]
fn a_dry_run_over_a_working_feed_reports_nothing() {
    let m = Membership::new();
    let (f, _) = feed(&m, Some("btc-5m"), |_| Ok(vec!["UP", "DOWN"]));
    let mut rt = RecorderRuntime::new(m, vec![Stream::Book]);
    rt.add_feed(f);

    let failures = dry_run_failures(&rt.tick(1_000));
    assert!(failures.is_empty(), "{failures:?}");
}

/// **The false green this whole change exists to remove.** The exact the CI box measurement: the
/// resolver refuses, `--once` printed one WARN and exited 0, and an operator scripting
/// `--once && systemctl enable --now` got a green light on a total resolve failure.
#[test]
fn a_dry_run_over_an_unresolvable_feed_reports_the_venue_and_the_reason() {
    let m = Membership::new();
    let (f, _) = feed(&m, Some("btc-5m"), |_| Err("network: io: Connection refused".into()));
    let mut rt = RecorderRuntime::new(m, vec![Stream::Book]);
    rt.add_feed(f);

    let failures = dry_run_failures(&rt.tick(1_000));
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("polymarket"), "the VENUE is named: {}", failures[0]);
    assert!(
        failures[0].contains("Connection refused"),
        "…and the resolver's own reason, so a three-venue box is diagnosable from this one \
             line: {}",
        failures[0]
    );
}

/// The zero-symbol case fails the dry run too, and the message must carry the rotation caveat —
/// the one accepted false positive, named rather than papered over.
#[test]
fn a_dry_run_over_a_family_that_resolves_to_nothing_fails_with_the_rotation_caveat() {
    let m = Membership::new();
    let (f, _) = feed(&m, Some("btc-5m"), |_| Ok(vec![]));
    let mut rt = RecorderRuntime::new(m, vec![Stream::Book]);
    rt.add_feed(f);

    let failures = dry_run_failures(&rt.tick(1_000));
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("NO live subscription"), "{}", failures[0]);
    assert!(failures[0].contains("re-run"), "the caveat is in the MESSAGE: {}", failures[0]);
}

/// The third arm: the venue accepted the resolve and then REFUSED the subscribe. Built as data
/// rather than driven through a client, because [`dry_run_failures`] is pure and the arm is
/// about the REPORT, not about how the report was produced.
///
/// `ReconcileReport::failed`'s own doc says a non-empty list is not yet a fault for a running
/// daemon — it is retried next pass — and that is exactly why a DRY RUN must judge it: a dry
/// run has one pass, and the operator is standing there.
#[test]
fn a_dry_run_fails_when_the_venue_refused_a_subscribe() {
    let tick = FeedTick::Reconciled {
        venue: "binance".into(),
        symbols: 1,
        report: ReconcileReport {
            started: vec![("BTCUSDT".into(), Stream::Trades)],
            failed: vec![("BTCUSDT".into(), Stream::Book, "socket reconnecting".into())],
            ..Default::default()
        },
        live: 1,
        last_nonempty_ms: Some(1_000),
    };

    let failures = dry_run_failures(&[tick]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("BTCUSDT"), "{}", failures[0]);
    assert!(failures[0].contains("socket reconnecting"), "{}", failures[0]);
}

/// A three-venue profile with one dead venue must FAIL, and the passing venues must not hide
/// it — "any venue resolved" is the weaker predicate that would pass this exact profile.
#[test]
fn a_dry_run_fails_on_one_dead_venue_among_healthy_ones() {
    let m = Membership::new();
    let (good, _) = feed(&m, Some("good"), |_| Ok(vec!["UP"]));
    let (bad, _) = feed(&m, Some("bad"), |_| Err("down".into()));
    let (also_good, _) = feed(&m, Some("also"), |_| Ok(vec!["DOWN"]));
    let mut rt = RecorderRuntime::new(m, vec![Stream::Book]);
    rt.add_feed(good);
    rt.add_feed(bad);
    rt.add_feed(also_good);

    let failures = dry_run_failures(&rt.tick(1_000));
    assert_eq!(failures.len(), 1, "exactly the dead one: {failures:?}");
}

#[test]
fn stop_all_releases_every_feed() {
    let m = Membership::new();
    let (a, _) = feed(&m, Some("f1"), |_| Ok(vec!["UP", "DOWN"]));
    let mut rt = RecorderRuntime::new(m.clone(), vec![Stream::Quotes, Stream::Book]);
    rt.add_feed(a);
    rt.tick(1_000);
    assert_eq!(rt.subscription_count(), 4);

    rt.stop_all();

    assert_eq!(rt.subscription_count(), 0);
}

// -- the two-phase stop ------------------------------------------------------------------------

/// How long one simulated socket takes to notice a raised stop flag — the fake's stand-in for a
/// venue's read timeout (2 s in production; `crates/vike-bridge-core/src/pump_spec.rs`'s
/// `market_pump_spec`). Small enough to keep the test quick, large enough that twelve of them
/// SERIALLY (1.8 s) is unmistakably distinct from one (150 ms) on a loaded runner.
const WIND_DOWN: Duration = Duration::from_millis(150);

/// A client that models the one property that makes the teardown cost what it costs: a socket
/// starts winding down when its flag is RAISED, and a join blocks until that wind-down is over.
///
/// So `unsubscribe` sleeps until `raised_at + WIND_DOWN` — no longer. Raise every flag first and
/// the whole set costs ONE wind-down; raise-and-join one at a time and it costs N. That is the
/// entire finding, expressed as something a clock can measure.
struct WindDownClient {
    next: u64,
    raised_at: Option<Instant>,
    /// Shared teardown trace: `"raise"` / `"join"`, in the order they happened, across ALL feeds.
    trace: Rc<RefCell<Vec<&'static str>>>,
}

impl DataClient for WindDownClient {
    fn subscribe_bars(&mut self, _s: &str, _i: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("no bars"))
    }
    fn subscribe_quotes(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.next += 1;
        Ok(SubscriptionId(self.next))
    }
    fn subscribe_trades(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.subscribe_quotes(s)
    }
    fn subscribe_book(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.subscribe_quotes(s)
    }
    fn begin_shutdown(&mut self) {
        self.trace.borrow_mut().push("raise");
        self.raised_at.get_or_insert_with(Instant::now);
    }
    fn unsubscribe(&mut self, _id: SubscriptionId) {
        self.trace.borrow_mut().push("join");
        // A join on a socket whose flag was never raised waits the FULL wind-down starting now —
        // exactly what the one-loop teardown paid, once per socket.
        let raised = *self.raised_at.get_or_insert_with(Instant::now);
        let ready = raised + WIND_DOWN;
        let now = Instant::now();
        if now < ready {
            std::thread::sleep(ready - now);
        }
    }
    fn shutdown(&mut self) {
        self.begin_shutdown();
    }
}

struct WindDownFeed {
    venue: String,
    client: WindDownClient,
}

impl VenueFeed for WindDownFeed {
    fn venue(&self) -> &str {
        &self.venue
    }
    fn family(&self) -> Option<&str> {
        None
    }
    fn desired(&mut self, _now_ms: i64) -> Result<BTreeSet<String>, String> {
        Ok(["A", "B", "C"].iter().map(|s| s.to_string()).collect())
    }
    fn client(&mut self) -> &mut dyn DataClient {
        &mut self.client
    }
}

/// ⚠ **The teardown-cost finding, as a measurement.** Twelve sockets across two venues: the stop
/// must cost about ONE wind-down, not twelve.
///
/// The one-loop `stop_all` (unsubscribe-and-join, one subscription at a time) paid a read
/// timeout PER SOCKET — 2 s each in production — outside the daemon's bounded region, so a
/// growing profile walked the recorder's total stop time through its unit's `TimeoutStopSec=`
/// and SIGKILL landed on the final Parquet flush.
///
/// MUTATION PROOF: delete the phase-1 loop in [`RecorderRuntime::stop_all`] and this goes red on
/// the elapsed assertion (12 wind-downs ≈ 1.8 s against a 450 ms ceiling); the ordering
/// assertion below goes red at the same time, and it is the one that cannot be flaky.
#[test]
fn stop_all_raises_every_flag_before_it_joins_anything() {
    let trace = Rc::new(RefCell::new(Vec::new()));
    let mut rt = RecorderRuntime::new(Membership::new(), vec![Stream::Quotes, Stream::Trades]);
    for venue in ["venue-a", "venue-b"] {
        rt.add_feed(Box::new(WindDownFeed {
            venue: venue.into(),
            client: WindDownClient { next: 0, raised_at: None, trace: Rc::clone(&trace) },
        }));
    }
    rt.tick(1_000);
    assert_eq!(rt.subscription_count(), 12, "3 symbols x 2 streams x 2 venues");

    let started = Instant::now();
    rt.stop_all();
    let elapsed = started.elapsed();

    assert_eq!(rt.subscription_count(), 0, "every subscription must still be released");

    // The ordering is the property; the clock is the consequence. Every raise must precede
    // every join, ACROSS feeds — a per-feed raise-then-join would give the parallelism back one
    // venue at a time and this assertion is what notices.
    let trace = trace.borrow();
    let first_join = trace.iter().position(|s| *s == "join").expect("joins must have happened");
    let last_raise = trace.iter().rposition(|s| *s == "raise").expect("raises must have happened");
    assert!(
        last_raise < first_join,
        "every feed's stop flag must be RAISED before the first join — trace: {trace:?}"
    );

    assert!(
        elapsed < WIND_DOWN * 3,
        "twelve sockets must cost about ONE wind-down ({WIND_DOWN:?}), not twelve: took \
             {elapsed:?}. In production that timeout is 2 s per socket, and the difference is \
             whether the recorder's stop fits inside its unit's TimeoutStopSec= before SIGKILL cuts \
             the flush."
    );
    assert!(
        elapsed >= WIND_DOWN,
        "…and the fake must actually have simulated a wind-down: took {elapsed:?}"
    );
}
