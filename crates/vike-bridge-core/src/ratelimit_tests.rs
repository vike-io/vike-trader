use super::*;

#[test]
fn allows_up_to_n_then_throttles() {
    let g = RateGate::new(3, Duration::from_secs(10));
    assert!(g.try_proceed());
    assert!(g.try_proceed());
    assert!(g.try_proceed());
    assert!(!g.try_proceed(), "the 4th within the window is throttled");
}

#[test]
fn clone_shares_the_same_window() {
    let g = RateGate::new(1, Duration::from_secs(10));
    let g2 = g.clone();
    assert!(g.try_proceed());
    assert!(!g2.try_proceed(), "a clone rides the same quota, not its own");
}

#[test]
fn slot_frees_after_the_window_elapses() {
    let g = RateGate::new(1, Duration::from_millis(100));
    assert!(g.try_proceed());
    assert!(!g.try_proceed());
    std::thread::sleep(Duration::from_millis(160));
    assert!(g.try_proceed(), "the window elapsed, so the one slot is free again");
}

#[test]
fn proceed_blocks_until_a_slot_frees() {
    let g = RateGate::new(1, Duration::from_millis(120));
    g.proceed(); // take the only slot immediately
    let t = Instant::now();
    g.proceed(); // must block ~window until the first ages out
    let waited = t.elapsed();
    assert!(
        waited >= Duration::from_millis(90),
        "proceed should block ~one window; waited {waited:?}"
    );
}

#[test]
fn window_slides_occurrences_age_out_individually() {
    let g = RateGate::new(2, Duration::from_millis(200));
    assert!(g.try_proceed()); // t≈0
    std::thread::sleep(Duration::from_millis(100));
    assert!(g.try_proceed()); // t≈100
    assert!(!g.try_proceed(), "two in the window → throttled");
    std::thread::sleep(Duration::from_millis(140)); // t≈240: only the t≈0 one aged out
    assert!(g.try_proceed(), "exactly one slot freed as the window slid");
    assert!(!g.try_proceed(), "the t≈100 occurrence still occupies the second slot");
}

// --- weighted costs --------------------------------------------------------------------------

#[test]
fn cost_n_consumes_n_slots() {
    let g = RateGate::new(5, Duration::from_secs(10));
    assert!(g.try_proceed_cost(3), "cost-3 fits in a fresh 5-slot window");
    // 3 of 5 taken; exactly 2 remain, provable by two cost-1 calls then a throttle.
    assert!(g.try_proceed(), "1st of the 2 remaining slots");
    assert!(g.try_proceed(), "2nd of the 2 remaining slots");
    assert!(!g.try_proceed(), "all 5 slots consumed (3 + 1 + 1) — the 6th throttles");
}

#[test]
fn cost_one_is_identical_to_plain_try_proceed() {
    // try_proceed IS try_proceed_cost(1): mixing the two folds into one shared window.
    let g = RateGate::new(2, Duration::from_secs(10));
    assert!(g.try_proceed_cost(1));
    assert!(g.try_proceed(), "the plain call rides the same window as the weighted cost-1");
    assert!(!g.try_proceed(), "2-slot window full after two cost-1 reservations");
    assert!(!g.try_proceed_cost(1), "the weighted cost-1 path throttles identically");
}

#[test]
fn failed_cost_n_reserves_nothing() {
    // All-or-nothing: a cost that does not fit must leave the window untouched (no partial take).
    let g = RateGate::new(5, Duration::from_secs(10));
    assert!(g.try_proceed_cost(4), "4 of 5 taken");
    assert!(!g.try_proceed_cost(2), "cost-2 won't fit in the 1 free slot — reserve nothing");
    assert!(g.try_proceed(), "the lone free slot survived the failed cost-2 (nothing was nibbled)");
    assert!(!g.try_proceed(), "now genuinely full (4 + 1)");
}

#[test]
fn proceed_cost_returns_immediately_when_the_cost_fits() {
    let g = RateGate::new(5, Duration::from_secs(10));
    let t = Instant::now();
    g.proceed_cost(3); // fits in the empty window — must not block
    assert!(t.elapsed() < Duration::from_millis(50), "a fitting cost must return promptly");
    assert!(g.try_proceed_cost(2), "3 taken, 2 free");
    assert!(!g.try_proceed(), "5/5 consumed");
}

#[test]
fn cost_n_blocks_until_n_slots_free_not_just_one() {
    // window=200ms, capacity=2, staggered so the two occupied slots age out 100ms apart. A cost-2
    // must wait for the SECOND (~+200ms), not merely the first (~+100ms, which a cost-1 would take).
    let g = RateGate::new(2, Duration::from_millis(200));
    g.proceed(); // slot A at t≈0
    std::thread::sleep(Duration::from_millis(100));
    g.proceed(); // slot B at t≈100 — window now full (2/2)
    let t = Instant::now(); // ≈100
    g.proceed_cost(2); // needs BOTH free: A ages out ≈200, B ≈300 → unblocks ≈300
    let waited = t.elapsed();
    assert!(
        waited >= Duration::from_millis(160),
        "cost-2 waits for the 2nd occurrence to age out (~200ms), not just the 1st (~100ms); \
             waited {waited:?}"
    );
}

#[test]
fn cost_above_capacity_is_clamped_not_unsatisfiable() {
    // A cost larger than the whole window must not deadlock: it clamps to capacity and takes the
    // entire window (and, when empty, returns without blocking).
    let g = RateGate::new(3, Duration::from_secs(10));
    let t = Instant::now();
    g.proceed_cost(9); // clamps to 3, the whole window; must not block on an empty gate
    assert!(
        t.elapsed() < Duration::from_millis(50),
        "an over-capacity cost on an empty gate returns"
    );
    assert!(!g.try_proceed(), "the window is now fully consumed");
}

// --- KeyedRateGate ---------------------------------------------------------------------------

fn wide() -> Duration {
    Duration::from_secs(30)
}

#[test]
fn keyed_quota_applies_per_key() {
    let g = KeyedRateGate::new("okx", vec![("subscribe", RateGate::new(2, wide()))], None);
    assert!(g.try_gate("subscribe"));
    assert!(g.try_gate("subscribe"));
    assert!(!g.try_gate("subscribe"), "3rd subscribe within the window is throttled");
}

#[test]
fn unlisted_key_is_ungated_without_a_default() {
    let g = KeyedRateGate::new("okx", vec![("subscribe", RateGate::new(1, wide()))], None);
    // no "order" gate and no default → always allowed, never throttles
    assert!(g.try_gate("order"));
    assert!(g.try_gate("order"));
    assert!(g.try_gate("order"));
}

#[test]
fn default_is_one_shared_bucket_for_unlisted_keys() {
    let g = KeyedRateGate::new("okx", vec![], Some(RateGate::new(1, wide())));
    assert!(g.try_gate("anything"));
    // the default is a SINGLE shared gate, so a different unlisted key hits the same window
    assert!(!g.try_gate("something-else"), "default bucket is shared across unlisted keys");
}

#[test]
fn distinct_keys_have_independent_windows() {
    let g = KeyedRateGate::new(
        "okx",
        vec![("login", RateGate::new(1, wide())), ("subscribe", RateGate::new(1, wide()))],
        None,
    );
    assert!(g.try_gate("login"));
    assert!(g.try_gate("subscribe"), "subscribe has its own window, unaffected by login");
    assert!(!g.try_gate("login"));
    assert!(!g.try_gate("subscribe"));
}

#[test]
fn clone_shares_the_keyed_windows() {
    let g = KeyedRateGate::new("okx", vec![("subscribe", RateGate::new(1, wide()))], None);
    let g2 = g.clone();
    assert!(g.try_gate("subscribe"));
    assert!(!g2.try_gate("subscribe"), "a clone rides the same per-key windows");
}

#[test]
fn keyed_cost_consumes_multiple_slots_of_its_key() {
    let g = KeyedRateGate::new("okx", vec![("subscribe", RateGate::new(4, wide()))], None);
    assert!(g.try_gate_cost("subscribe", 3), "cost-3 fits in the 4-slot subscribe window");
    assert!(
        !g.try_gate_cost("subscribe", 2),
        "only 1 slot left — cost-2 throttles (nothing taken)"
    );
    assert!(g.try_gate("subscribe"), "the lone remaining slot still admits a cost-1 send");
    assert!(!g.try_gate("subscribe"), "subscribe window now full (3 + 1)");
}

#[test]
fn keyed_cost_on_an_ungated_key_is_always_allowed() {
    let g = KeyedRateGate::new("okx", vec![("subscribe", RateGate::new(1, wide()))], None);
    // no "order" gate and no default → the weighted gate is a pure passthrough, any cost allowed.
    assert!(g.try_gate_cost("order", 100));
    assert!(g.try_gate_cost("order", 100));
}

#[test]
fn keyed_cost_one_matches_plain_try_gate() {
    let g = KeyedRateGate::new("okx", vec![("subscribe", RateGate::new(2, wide()))], None);
    assert!(g.try_gate_cost("subscribe", 1));
    assert!(g.try_gate("subscribe"), "plain gate rides the same window as the weighted cost-1");
    assert!(!g.try_gate("subscribe"), "2-slot window full after two cost-1 sends");
}

/// How often the competing cost-1 caller talks in the two starvation tests, against their shared
/// 4-per-400 ms gate.
///
/// ⚠ **Load-bearing: the chatter must stay far enough inside quota that it never blocks on its
/// own account**, because a blocked chatter joins the queue, stops taking slots, and hands the
/// heavy caller its window, turning a broken gate green. 150 ms is 2.67 of 4 slots (67 %); 120 ms
/// (83 %) self-queues, and with it a deliberately broken fast-path guard stayed GREEN.
const CHATTER_PERIOD: Duration = Duration::from_millis(150);

/// How long a readiness spin may wait for a caller to show up as [`blocked`](RateGate::blocked)
/// before the test fails by name instead of hanging. A BACKSTOP (the spins finish in
/// microseconds) that must stay UNDER the window of the gate it spins against: past the window the
/// gate serves, `blocked()` falls, and the spin's condition becomes unreachable.
const SETUP_BUDGET: Duration = Duration::from_millis(1_500);

/// A zero-occurrence gate is refused at construction: it would admit everything, forever, while
/// presenting as a limiter. Pins the `assert!` (not `debug_assert!`) in [`RateGate::new`].
#[test]
#[should_panic(expected = "at least one occurrence")]
fn a_zero_occurrence_gate_is_refused_rather_than_failing_open() {
    let _ = RateGate::new(0, Duration::from_secs(1));
}

/// **A cost-`n` caller must not be starved by a stream of cost-1 callers that are themselves
/// inside quota** (module doc's fairness rule).
///
/// Without the queue, a woken waiter re-checks after a fresh cost-1 arrival has already taken the
/// slot, and since a cost-`n` reservation needs `n` SIMULTANEOUSLY free slots
/// ([`RateGate::reserve_cost_if_free`]), a trickle keeping ONE slot live means the heavy caller
/// completes only when the contention stops. Under the fair gate it completes in about one window.
/// Only a test with two callers in CONTENTION can see this; every other test here is single-caller.
/// On a gate shared by a transport and a backfill the starved caller can be an ORDER.
#[test]
fn a_heavy_caller_is_not_starved_by_a_stream_of_light_ones() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let g = RateGate::new(4, Duration::from_millis(400));
    // Seed one occurrence BEFORE either thread starts, or a heavy caller winning the first lock
    // takes an empty gate and the test passes having contended with nothing (a false green that
    // once made a broken gate look fixed).
    assert!(g.try_proceed(), "the gate is contended before the heavy caller ever arrives");
    let stop = Arc::new(AtomicBool::new(false));
    let chatter = {
        let (g, stop) = (g.clone(), Arc::clone(&stop));
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                g.proceed(); // cost-1, comfortably inside 4-per-400ms
                std::thread::sleep(CHATTER_PERIOD);
            }
        })
    };
    let t = Instant::now();
    let heavy = {
        let g = g.clone();
        std::thread::spawn(move || {
            g.proceed_cost(4);
            t.elapsed()
        })
    };
    // Four windows is far longer than any honest wait for this gate.
    std::thread::sleep(Duration::from_millis(1_600));
    let finished_while_contended = heavy.is_finished();
    stop.store(true, Ordering::Relaxed);
    let waited = heavy.join().unwrap();
    chatter.join().unwrap();
    assert!(
        finished_while_contended,
        "a cost-4 caller was still blocked after 1600 ms (4x the window) while a cost-1 stream \
             that is itself inside quota kept talking; it completed at {waited:?}, i.e. only once \
             the contention stopped. A gate that starves its heavy caller indefinitely delays \
             whatever shares it — on the exec path, an order."
    );
}

/// **The sibling above, driven through the door production uses**: every real blocking call site
/// goes through [`RateGate::proceed_logged`] / [`RateGate::proceed_cost_logged`] (try first, warn
/// once, then block). The try half is [`RateGate::try_proceed`], and a NON-BLOCKING probe allowed
/// to take a free slot past a blocked caller re-creates the starvation through a second door, so
/// a fix that queues only `proceed_cost` turns the sibling green and leaves this one red.
#[test]
fn a_heavy_caller_is_not_starved_through_the_logged_door() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let g = RateGate::new(4, Duration::from_millis(400));
    // Seed one occurrence BEFORE either thread starts (see the sibling above).
    assert!(g.try_proceed(), "the gate is contended before the heavy caller ever arrives");
    let stop = Arc::new(AtomicBool::new(false));
    let chatter = {
        let (g, stop) = (g.clone(), Arc::clone(&stop));
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                g.proceed_logged("test", "chatter"); // try_proceed, then block — the LEAN pattern
                std::thread::sleep(CHATTER_PERIOD);
            }
        })
    };
    let t = Instant::now();
    let heavy = {
        let g = g.clone();
        std::thread::spawn(move || {
            g.proceed_cost_logged("test", "resync", 4);
            t.elapsed()
        })
    };
    std::thread::sleep(Duration::from_millis(1_600));
    let finished_while_contended = heavy.is_finished();
    stop.store(true, Ordering::Relaxed);
    let waited = heavy.join().unwrap();
    chatter.join().unwrap();
    assert!(
        finished_while_contended,
        "a cost-4 caller was still blocked after 1600 ms (4x the window) while a cost-1 stream \
             rode the try-then-block path every venue transport uses; it completed at {waited:?}. \
             Queueing only the BLOCKING path leaves the non-blocking probe barging past it."
    );
}

/// **The rule behind the two tests above, deterministic**: while a caller is blocked waiting for
/// room, a non-blocking probe must not take a slot, **even when it would fit**. Free capacity is
/// not the question; arrival order is.
#[test]
fn a_probe_yields_to_a_caller_already_blocked() {
    // ONE occurrence of four: room for a cost-1, never for the waiting cost-4. ⚠ No sleep between
    // setup and assertion: the probe fires the instant `blocked()` proves the heavy caller queued
    // (a clock-aged window could be drifted past under parallel nextest, flipping the answer).
    // ⚠ The window (2 s) must exceed [`SETUP_BUDGET`], or a slow spawn lets the heavy caller
    // complete and the spin fails spuriously.
    let g = RateGate::new(4, Duration::from_millis(2_000));
    assert!(g.try_proceed(), "one occurrence taken; three of four slots stay free");
    let heavy = {
        let g = g.clone();
        std::thread::spawn(move || g.proceed_cost(4))
    };
    // A bounded spin: past the window `blocked()` would sit at 0 and an unbounded spin would hang.
    let setup = Instant::now();
    while g.blocked() == 0 {
        assert!(
            setup.elapsed() < SETUP_BUDGET,
            "the cost-4 caller never showed up as blocked within {SETUP_BUDGET:?}"
        );
        std::thread::yield_now();
    }
    assert!(
        !g.try_proceed(),
        "3 of 4 slots are free, but a cost-4 caller queued first. Admitting this probe is \
             precisely what starves it: the probe's occurrence keeps the window from ever showing \
             4 free at once."
    );
    heavy.join().unwrap(); // unblocks once the lone occurrence ages out
}

/// **The queue is FIFO, not merely "a queue"**: a gate releasing waiters in arbitrary order is
/// still unbounded for whichever keeps losing the race, which "not starved" alone cannot catch.
///
/// * **Arrival order is established, not assumed.** Each thread is confirmed BLOCKED via
///   [`RateGate::blocked`] before the next spawns. `blocked()` is `next_ticket - serving` and only
///   waiters `0..=i` exist at the i-th spin, so `blocked() >= i + 1` holds ONLY when
///   `serving == 0` and `next_ticket == i + 1`: waiter `i` provably holds ticket `i`. A setup
///   that outran the window sees the count FALL and fails by name on [`SETUP_BUDGET`].
/// * **⚠ Service order is read from the gate** ([`State::served_tickets`]): a `Vec` the waiters
///   push to after `proceed_cost` returns records scheduler PUSH order and goes red on a fair
///   gate.
/// * **⚠ The waiters are NOT interchangeable, which gives the test teeth.** With equal cost-1
///   waiters an unfair gate merely races, FIFO-biased by timer order (a deleted head guard was
///   caught only 4 runs in 10). So ticket 0 wants 4 against 3 free slots and tickets 1-3 want 1
///   each: a fair gate serves nobody until the window opens, an unfair one lets the light callers
///   through at once. That is also the production hazard: light chatter walking past a heavy
///   resync burst.
#[test]
fn blocked_callers_are_served_in_arrival_order() {
    // Five of eight slots taken in ONE go, so they age out together and the queue drains in one
    // opening. The 3 free slots fit every cost-1 caller behind the head and never the cost-4
    // head, for the whole setup: "nothing served yet" is STRUCTURAL for a fair gate.
    //
    // ⚠ The window must outlast setup: the spin waits for `blocked()` to COUNT UP, and a head
    // served mid-setup makes it fall. The deadline is ONE cumulative clock hoisted out of the
    // loop and budgeted under the window (a per-iteration clock let four iterations outrun it).
    let g = RateGate::new(8, Duration::from_millis(2_000));
    assert!(g.try_proceed_cost(5), "5 of 8 taken: room for a cost-1, never for the cost-4 head");
    let mut waiters = Vec::new();
    let setup = Instant::now();
    // Ticket 0 is the head and cannot fit; tickets 1..=3 each fit RIGHT NOW and must still wait.
    for (i, cost) in [4usize, 1, 1, 1].into_iter().enumerate() {
        let gate = g.clone();
        waiters.push(std::thread::spawn(move || gate.proceed_cost(cost)));
        // Everyone spawned so far must be queued before the next arrives (pins waiter `i` to
        // ticket `i`); bound to a name because clippy's `int_plus_one` rejects `>= i + 1`.
        let all_queued = i + 1;
        // ⚠ A `loop`, not a `while`: both guards must run at least once per waiter, and the barge
        // rule is the one an unfair gate trips first.
        loop {
            assert!(
                setup.elapsed() < SETUP_BUDGET,
                "waiter {i} never showed up as blocked within {SETUP_BUDGET:?} of setup start — \
                     the setup outran the window, so this run proves nothing about ordering"
            );
            // The FIFO rule's teeth, checked while the queue assembles: an overtaking gate has
            // already served a cost-1 caller by now. Named here, or it would surface as the
            // budget message and blame a slow box for unfairness.
            assert!(
                g.served_tickets().is_empty(),
                "the gate served ticket(s) {:?} while ticket 0 was queued and could not fit. \
                     3 of 8 slots are free, so every cost-1 caller behind the head fits — and \
                     admitting one is exactly what starves the head: its occurrence keeps the window \
                     from ever showing the 4 free at once that the head is waiting for.",
                g.served_tickets()
            );
            if g.blocked() >= all_queued {
                break;
            }
            std::thread::yield_now();
        }
        // Waiter `i` now provably holds ticket `i` (this test's doc).
    }
    for w in waiters {
        w.join().unwrap();
    }
    // The gate's OWN record, written under the reservation lock.
    assert_eq!(
        g.served_tickets(),
        vec![0u64, 1, 2, 3],
        "blocked callers must be served in ARRIVAL order: waiter i queued i-th and so holds \
             ticket i, and the gate must hand the window to the tickets in ascending order. A \
             permutation here means a caller that arrived later reserved first, which leaves \
             whoever keeps losing that race unbounded — the starvation the queue exists to prevent, \
             wearing a different face."
    );
}
