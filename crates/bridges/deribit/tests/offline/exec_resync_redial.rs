//! The deribit audit-A3 RESYNC socket's lifecycle, over the local WebSocket stand-in in
//! `crate::fake_deribit_ws` — no venue, no credentials, no `#[ignore]`.
//!
//! **The defect these pin is the quietest of the three.** `crates/bridges/deribit/src/exec.rs`'s
//! `run` opens a SECOND authed order-WS for the pump's A3 resync supervisor, calls `connect()` on
//! it exactly once, and — like every other owner in this crate until 2026-08-25 — never re-dialled
//! it. `crates/bridges/deribit/src/transport.rs`'s `DeribitOrderTransport::call` propagates a
//! failure and KEEPS the dead socket, so one venue-side close disabled the post-reconnect replay
//! for the life of the process. The fetch errors were then swallowed by `unwrap_or_else`, so
//! nothing was logged, nothing errored, and no operator could see it: the A3 replay exists to
//! recover order/trade activity that landed inside a WS reconnect gap, so what a dead resync
//! socket produces is **missing fills**, not an alert.
//!
//! ⚠ **The cure here is the RECON one — re-dial AND re-send — and that is a decision, not a
//! copy.** The licence is that this socket carries `public/auth` plus exactly two idempotent
//! reads (`private/get_order_history_by_instrument`, `private/get_user_trades_by_instrument`) and
//! nothing else, so a retry cannot double anything. The ORDER socket's twin problem has the
//! OPPOSITE answer — re-dial, never re-send, re-QUERY — because a resent `private/buy` is a second
//! real order; that is `exec_ambiguous_submit.rs`. The recon socket's own is
//! `recon_client_redial.rs`. `vike_deribit::exec::A3Resync`'s doc argues the split.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Level, Metadata, Subscriber};

use vike_deribit::client::DeribitRest;
use vike_deribit::exec::A3Resync;
use vike_model::events::Event;
use vike_model::SymbolProperties;

use crate::fake_deribit_ws::{
    transport_against, FakeDeribit, Session, RESYNC_CANCEL_COID, RESYNC_FILL_COID, RESYNC_TRADE_ID,
    SYMBOL,
};

/// The target `A3Resync` logs its two transition lines on. Spelled here rather than imported: a
/// test that read the value out of the code it is checking would agree with any typo.
const TARGET: &str = "vike_deribit::exec";

/// ⚠ **Every test in this module takes this**, and the reason is a `tracing` property rather than
/// anything about the code under test.
///
/// `tracing`'s per-callsite `Interest` is cached PROCESS-GLOBALLY and written by whichever thread
/// reaches a callsite FIRST: `tracing_core::callsite`'s `rebuild_callsite_interest` asks only that
/// thread's current dispatcher, and a scoped `tracing::subscriber::with_default` never registers
/// with the dispatcher list at all (`set_default` does not call `register_dispatch`). So a thread
/// with no subscriber installed caches `Interest::never()` for that line — permanently, since
/// nothing re-registers it — and the grouped `offline` binary runs its tests as concurrent
/// THREADS. A sibling test here that heals a socket outside a capture can therefore disable
/// `A3Resync`'s `info!` line for the capture tests.
///
/// MEASURED, not theorised: `the_recovery_is_logged_once_and_a_healthy_lane_stays_silent` passed
/// under `cargo test -p vike-deribit --test offline the_recovery` and failed in the full run, with
/// its WARN captured and its INFO gone. Serialising this module is half the cure; [`capture`]
/// rebuilding the cache with its own subscriber installed is the other half, and neither works
/// alone. Only this module's tests can reach those two callsites, so this lock is sufficient.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    // A panicking test poisons the mutex, and every later test would then fail for the wrong
    // reason — the guarded state is the tracing cache, which a panic does not corrupt.
    SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// An `A3Resync` over the fake, built the way `crates/bridges/deribit/src/exec.rs`'s `run` builds
/// the real one — its OWN dedicated authed transport, owned outright (owning it is what licenses
/// the re-dial).
fn resync_against(fake: &FakeDeribit) -> A3Resync {
    A3Resync::new(
        DeribitRest::new(transport_against(fake), SYMBOL, SymbolProperties::default(), "BTC"),
        SYMBOL,
    )
}

/// The replayed events, flattened to what these tests assert on.
fn kinds(evs: &[Event]) -> Vec<String> {
    evs.iter()
        .map(|e| match e {
            Event::Fill(f) => format!("Fill:{}:{}", f.client_order_id, f.trade_id),
            Event::OrderFilled(w) => format!("OrderFilled:{}", w.client_order_id),
            Event::OrderPartiallyFilled(w) => format!("OrderPartiallyFilled:{}", w.client_order_id),
            Event::OrderCanceled(w) => format!("OrderCanceled:{}", w.client_order_id),
            Event::OrderRejected(w) => format!("OrderRejected:{}", w.client_order_id),
            other => format!("other:{other:?}"),
        })
        .collect()
}

/// What a COMPLETE replay looks like: the user-trades half (a filled trade → bare `Fill` + its
/// `OrderFilled` wrap) THEN the order-history half (the cancelled order's non-fill terminal; the
/// scripted OPEN row maps to nothing). Both halves are named on purpose — a heal that recovered
/// only the read that happened to fail would show up here as a short list.
fn full_replay() -> Vec<String> {
    vec![
        format!("Fill:{RESYNC_FILL_COID}:{RESYNC_TRADE_ID}"),
        format!("OrderFilled:{RESYNC_FILL_COID}"),
        format!("OrderCanceled:{RESYNC_CANCEL_COID}"),
    ]
}

// -------------------------------------------------------------------------------------------
// the heal
// -------------------------------------------------------------------------------------------

/// THE REGRESSION. The venue closes the socket BETWEEN the replay's two reads; the second read
/// must re-dial and complete rather than losing its half. Before the fix this pass returned the
/// order-history half only — a replay silently missing every fill in the gap — and every later
/// pass returned nothing at all, forever.
#[test]
fn a_close_between_the_two_reads_is_re_dialed_and_the_replay_stays_whole() {
    let _serial = serial();
    let fake = FakeDeribit::spawn(vec![Session::OkThenClose(1), Session::OkForever]);
    let mut resync = resync_against(&fake);

    let replayed = resync.history_events();

    assert_eq!(kinds(&replayed), full_replay(), "both halves survive the close");
    assert_eq!(fake.connections(), 2, "the resync socket was re-dialed exactly once");
    // The close landed BETWEEN the reads, so the second read's frame was written into a socket the
    // venue had already dropped and never reached it — one recorded call each. The RE-SEND is
    // therefore not observable here, which is why it has a test of its own below rather than an
    // assertion here that would quietly measure the wrong thing.
    assert_eq!(fake.count("private/get_order_history_by_instrument"), 1);
    assert_eq!(fake.count("private/get_user_trades_by_instrument"), 1);
}

/// ⚠ THE CURE, STATED AS A COUNT — and the exact mirror of `exec_ambiguous_submit.rs`'s
/// `the_order_is_never_re_sent_over_the_new_socket`: the SAME venue script, with the OPPOSITE
/// correct answer. The venue RECEIVES the request and dies without answering, so it may or may not
/// have served it — the ambiguous shape. On the ORDER socket that ambiguity forbids a re-send (a
/// second `private/buy` is a second real order) and forces a re-QUERY. On THIS socket there is
/// nothing to be ambiguous about: `private/get_order_history_by_instrument` moves nothing, so the
/// read is simply sent again and the replay comes back whole instead of half.
#[test]
fn an_ambiguous_read_is_re_sent_over_the_new_socket() {
    let _serial = serial();
    let fake = FakeDeribit::spawn(vec![Session::CloseBeforeAnswering, Session::OkForever]);
    let mut resync = resync_against(&fake);

    assert_eq!(kinds(&resync.history_events()), full_replay(), "the replay is whole");

    assert_eq!(
        fake.count("private/get_order_history_by_instrument"),
        2,
        "the venue received the READ twice — deliberate, and safe only because it is a read"
    );
    assert_eq!(fake.count("private/get_user_trades_by_instrument"), 1, "the other half, once");
    assert_eq!(fake.connections(), 2);
    // ...and the heal cannot have moved anything, at any count: none of these is a frame this
    // socket is able to carry.
    for order_method in ["private/buy", "private/sell", "private/cancel", "private/edit"] {
        assert_eq!(fake.count(order_method), 0, "{order_method} must never leave this socket");
    }
}

/// ...and the socket that died BETWEEN passes heals too — the shape a real reconnect gap takes,
/// since the supervisor fires one pass per pump re-open. Under the old code this second pass was
/// empty, and so was every pass after it for the life of the daemon.
#[test]
fn a_pass_after_the_socket_died_replays_everything_again() {
    let _serial = serial();
    let fake = FakeDeribit::spawn(vec![Session::OkThenClose(2), Session::OkForever]);
    let mut resync = resync_against(&fake);

    let first = resync.history_events(); // both reads served, then the venue closes
    assert_eq!(kinds(&first), full_replay());
    assert_eq!(fake.connections(), 1, "one dial so far");

    let second = resync.history_events();
    assert_eq!(kinds(&second), full_replay(), "the post-reconnect replay resumed");
    assert_eq!(fake.connections(), 2, "re-dialed exactly once");

    // ...and the healed socket keeps serving: the re-dial is not a one-shot.
    assert_eq!(kinds(&resync.history_events()), full_replay());
    assert_eq!(fake.connections(), 2, "a healthy socket is never re-dialed");
}

/// A venue that is genuinely GONE replays nothing, BOUNDED: one re-dial attempt per read, no
/// internal loop — the pump's own reconnect cadence is the retry cadence. This also covers the
/// socketLESS state a refused dial leaves behind (`connect()` closes the prior socket before it
/// dials), which must itself earn a re-dial or one unlucky reconnect would strand the replay just
/// as permanently as the original defect did.
#[test]
fn a_dead_venue_replays_nothing_bounded_rather_than_spinning() {
    let _serial = serial();
    let fake = FakeDeribit::spawn(vec![Session::OkThenClose(2)]);
    let mut resync = resync_against(&fake);

    assert_eq!(kinds(&resync.history_events()), full_replay(), "served, then the venue closes");
    fake.await_listener_closed();

    let started = Instant::now();
    let a = resync.history_events();
    let b = resync.history_events();
    let elapsed = started.elapsed();

    assert!(a.is_empty() && b.is_empty(), "a dead venue replays nothing: {a:?} {b:?}");
    assert!(
        elapsed < Duration::from_secs(10),
        "two dead passes took {elapsed:?} — a re-dial must be ONE bounded attempt, never a spin"
    );
    assert_eq!(fake.connections(), 1, "the refused dials never reached an accept");
}

/// PRECISION. A JSON-RPC error object is the venue ANSWERING — the socket is alive, and re-dialing
/// it would churn a fresh TCP+TLS+auth handshake against the credit pool
/// `crates/bridges/deribit/src/ratelimit.rs`'s `order_ws_gate` documents as zero-margin, while
/// hiding a real API refusal behind a reconnect that cannot fix it.
#[test]
fn a_venue_error_reply_does_not_re_dial() {
    let _serial = serial();
    // TWO sessions on purpose: a spurious re-dial would be ACCEPTED and show up in the count. With
    // a one-session script it would merely be refused, and this test would pass for the wrong
    // reason — the second session is what makes the count the real assertion.
    let fake = FakeDeribit::spawn(vec![Session::VenueErrorForever, Session::VenueErrorForever]);
    let mut resync = resync_against(&fake);

    assert!(resync.history_events().is_empty(), "the venue refused both reads");
    assert_eq!(fake.connections(), 1, "a live socket that answered is never re-dialed");

    assert!(resync.history_events().is_empty());
    assert_eq!(fake.connections(), 1);
    assert_eq!(
        fake.count("private/get_order_history_by_instrument"),
        2,
        "one attempt per pass — a refusal is not retried either"
    );
}

// -------------------------------------------------------------------------------------------
// the visibility
// -------------------------------------------------------------------------------------------

/// ⚠ The whole reason this path needed a log at all: its failure is otherwise INVISIBLE — the
/// fetch errors are swallowed into an empty replay, so a stale A3 lane looks exactly like an
/// account with no recent activity. One line per OUTAGE, not one per attempt: the recon lane's
/// incident logged 795 identical lines from a single close, and the cure must not reproduce that
/// at a different layer.
#[test]
fn the_outage_is_logged_once_not_once_per_failed_read() {
    let _serial = serial();
    let fake = FakeDeribit::spawn(vec![Session::OkThenClose(2)]);
    let mut resync = resync_against(&fake);
    let _ = resync.history_events(); // served
    fake.await_listener_closed();

    let events = capture(|| {
        for _ in 0..3 {
            let _ = resync.history_events(); // 6 failed reads in total
        }
    });

    let warns = at_target(&events, Level::WARN);
    assert_eq!(warns.len(), 1, "one line per outage, whatever the attempt count: {warns:?}");
    assert!(warns[0].contains("STALE"), "it names the consequence, not just the error: {warns:?}");
    assert!(
        at_target(&events, Level::INFO).is_empty(),
        "nothing recovered, so nothing announces a recovery"
    );
}

/// ...and the recovery closes the outage exactly once, so the pair of lines brackets it. Without
/// this the latch would be a one-way switch: an operator who saw the warn could never tell from
/// the log whether the lane came back.
#[test]
fn the_recovery_is_logged_once_and_a_healthy_lane_stays_silent() {
    let _serial = serial();
    let fake = FakeDeribit::spawn(vec![Session::OkThenClose(1), Session::OkForever]);
    let mut resync = resync_against(&fake);

    let events = capture(|| {
        assert_eq!(kinds(&resync.history_events()), full_replay(), "it healed mid-pass");
        let _ = resync.history_events(); // wholly healthy — must add nothing
        let _ = resync.history_events();
    });

    assert_eq!(at_target(&events, Level::WARN).len(), 1, "one outage");
    let recovered = at_target(&events, Level::INFO);
    assert_eq!(recovered.len(), 1, "one recovery: {recovered:?}");
    assert!(recovered[0].contains("live again"), "{recovered:?}");
}

/// ⚠ A latch that never RE-ARMS is the failure mode a "log once" rule invites, and it is worse
/// than logging per attempt: the first outage is reported, every one after it is silent forever,
/// and the lane looks healthier the longer it has been broken. So the pair of lines must bracket
/// EACH outage, not just the first.
#[test]
fn a_second_outage_after_a_recovery_is_visible_again() {
    let _serial = serial();
    let fake = FakeDeribit::spawn(vec![
        Session::OkThenClose(2),
        Session::OkThenClose(2),
        Session::OkForever,
    ]);
    let mut resync = resync_against(&fake);

    let events = capture(|| {
        for _ in 0..3 {
            // Pass 1 is served then closed on; passes 2 and 3 each heal a fresh close.
            assert_eq!(kinds(&resync.history_events()), full_replay());
        }
    });

    assert_eq!(at_target(&events, Level::WARN).len(), 2, "two outages, two warnings");
    assert_eq!(at_target(&events, Level::INFO).len(), 2, "...and two recoveries");
    assert_eq!(fake.connections(), 3, "one dial plus one per outage");
}

/// A HEALTHY lane logs nothing at all. Stated on its own because the cheapest way to pass the two
/// tests above is to log unconditionally and count — which would bury a real incident under one
/// line per reconnect on every mount that never had a problem.
#[test]
fn a_lane_that_never_fails_logs_nothing() {
    let _serial = serial();
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let mut resync = resync_against(&fake);

    let events = capture(|| {
        for _ in 0..3 {
            assert_eq!(kinds(&resync.history_events()), full_replay());
        }
    });

    assert!(
        at_target(&events, Level::WARN).is_empty() && at_target(&events, Level::INFO).is_empty(),
        "a working replay is silent: {events:?}"
    );
}

// -------------------------------------------------------------------------------------------
// the capture harness — the `tracing` facade only, no dev-dependency
// -------------------------------------------------------------------------------------------
//
// A `tracing-subscriber` dev-dep would cost a lockfile edit to prove two log lines, so this is the
// same hand-rolled collector `crates/vike-script/tests/script_print_is_not_stdout.rs` uses. It is
// installed as the THREAD's default, which is what makes it safe inside the grouped `offline`
// binary: cargo runs that binary's tests as threads in one process, and a global subscriber would
// be a process-wide race between them.

/// One captured event, flattened to the two things asserted on here.
#[derive(Clone, Debug)]
struct Ev {
    target: String,
    level: Level,
    message: String,
}

type Log = Arc<Mutex<Vec<Ev>>>;

struct Collector(Log);

impl Subscriber for Collector {
    fn enabled(&self, _m: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _a: &Attributes<'_>) -> Id {
        Id::from_u64(1) // `from_u64` panics on 0; no span is ever entered here
    }
    fn record(&self, _s: &Id, _v: &Record<'_>) {}
    fn record_follows_from(&self, _s: &Id, _f: &Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut v = MessageField::default();
        event.record(&mut v);
        self.0.lock().unwrap().push(Ev {
            target: event.metadata().target().to_string(),
            level: *event.metadata().level(),
            message: v.0,
        });
    }
    fn enter(&self, _s: &Id) {}
    fn exit(&self, _s: &Id) {}
}

/// Pulls the `message` field out of an event. It arrives as `format_args!` through `record_debug`
/// (a `&dyn Debug` whose `Debug` is its `Display`); `record_str` is implemented too, because
/// relying on one would silently record nothing if tracing routed the other.
#[derive(Default)]
struct MessageField(String);

impl Visit for MessageField {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_string();
        }
    }
}

/// Runs `f` with a fresh capturing subscriber installed as the THREAD's default.
fn capture(f: impl FnOnce()) -> Vec<Ev> {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    tracing::subscriber::with_default(Collector(Arc::clone(&log)), || {
        // ⚠ Load-bearing, and the reason is [`SERIAL`]'s: a callsite first reached by a thread with
        // no subscriber caches `Interest::never()` for the life of the PROCESS, and a scoped
        // `with_default` does not re-register it. This recomputes every registered callsite's
        // interest against the dispatcher that is current on THIS thread — which, inside this
        // closure, is the collector above. Without it a sibling test that happened to reach
        // `A3Resync`'s `info!` line first silently switched it off here, and the failure looked
        // like a missing log line rather than a filtered one.
        tracing::callsite::rebuild_interest_cache();
        f();
    });
    // ⚠ Two statements rather than a tail `log.lock().unwrap()...` — that spelling is E0597: a
    // `MutexGuard` temporary in a block's TAIL expression is dropped after the block's own locals.
    let mut events = Vec::new();
    events.extend(log.lock().unwrap().iter().cloned());
    events
}

/// The messages `A3Resync` itself emitted at `level`. Filtered by TARGET on purpose: the transport
/// logs its own warn/error on the very same failures, and counting those would make "logged once"
/// pass while this module logged per attempt.
fn at_target(events: &[Ev], level: Level) -> Vec<String> {
    events
        .iter()
        .filter(|e| e.target == TARGET && e.level == level)
        .map(|e| e.message.clone())
        .collect()
}
