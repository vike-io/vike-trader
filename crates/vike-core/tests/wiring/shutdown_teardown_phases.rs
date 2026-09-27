//! TEARDOWN COST gate: a multi-venue core's stop must raise EVERY engine's stop flag before it
//! joins the first, so the whole mount costs about ONE venue wind-down rather than one per venue.
//!
//! # The defect, as a cost
//!
//! `ExecutionEngine::shutdown` calls `ExecutionClient::detach`, and a venue's detach JOINS its
//! user-data pump — a thread that learns it should stop on its next stop-flag poll
//! (`crates/bridges/binance/src/family/listenkey.rs`'s `POLL` is 1s, and the recv loop in
//! `vike_bridge_core::run_user_data_forever_with_idle` re-checks the flag only on that cadence).
//! The one-loop teardown — detach-and-join, one engine at a time — therefore paid that wind-down
//! PER ENGINE, serially, on the shutdown path of every shipped binary, while a service-managed stop
//! was already counting against its unit's `TimeoutStopSec=`.
//!
//! This is the exec-plane twin of the finding `crates/vike-recorder/src/runtime.rs`'s `stop_all`
//! was fixed for one plane over, and this file is the twin of that function's own measurement test.
//!
//! # Why this file exists separately from the unit tests
//!
//! `crates/vike-exec/src/execution_engine/client.rs` proves the SEAM (the default is a no-op, a
//! boxed client delegates it). That says nothing about whether the core CALLS it, or in what order.
//! This file drives a real [`spawn_core_multi`] and stops it through the real `shutdown_and_join`
//! path, so the phase-one loop in `crates/vike-core/src/runtime/mod.rs`'s `CoreThread::run`
//! teardown is what is under test.
//!
//! MUTATION PROOF: delete that phase-one loop and
//! [`teardown_raises_every_engines_flag_before_it_joins_anything`] goes red on the ordering
//! assertion — the one that cannot be flaky — and on the elapsed assertion at the same time.
//!
//! # ⚠ What a green run here does NOT prove
//!
//! That any REAL venue overlaps its wind-down. The seam is a default no-op, so a bridge that does
//! not delegate `begin_detach` is silently unchanged by all of this — the double below wires it
//! precisely because the trait cannot force a venue to. Which bridges are wired is a per-venue
//! fact, not something this test can see.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, spawn_core_multi};
use vike_exec::{Account, BalanceMode, ExecutionClient, ExecutionEngine, RiskGate, RiskLimits};
use vike_model::OrderRequest;

type DynClient = Box<dyn ExecutionClient + Send>;

/// How long one simulated venue takes to notice a raised stop flag — the double's stand-in for a
/// user-data pump's stop-flag poll (1 s in production). Small enough to keep the test quick, large
/// enough that six of them SERIALLY (1.2 s) is unmistakably distinct from one (200 ms) on a loaded
/// runner.
const WIND_DOWN: Duration = Duration::from_millis(200);

/// Primary + extras. Six is set by the elapsed assertion's SLACK: its ceiling is
/// `(ENGINES - 1) × WIND_DOWN` (the least a serial teardown can cost, less one), a green run costs
/// about one `WIND_DOWN` plus thread overhead, so a loaded runner gets `(ENGINES - 2)` wind-downs of
/// headroom before a false red — four of them (800 ms) at six. Three engines would leave one
/// wind-down (200 ms) of slack, which a single scheduling hiccup on the CI box eats; more than six adds
/// no evidence (the ORDERING assertion is the gate, and it is exact at any N ≥ 2) and only lengthens
/// the serial run this gate refuses, which at six still finishes in about a second (1.2 s). That it
/// is also half the twelve venues a default `vike-app` build mounts is a coincidence, not the
/// argument.
const ENGINES: usize = 6;

/// A client that models the ONE property that makes this teardown cost what it costs: a venue's
/// threads start winding down when the flag is RAISED, and the detach blocks until that wind-down
/// is over.
///
/// So `detach` sleeps until `raised_at + WIND_DOWN` — no longer. Raise every flag first and the
/// whole mount costs ONE wind-down; raise-and-join one engine at a time and it costs N. That is the
/// entire finding, expressed as something a clock can measure.
struct WindDownClient {
    raised_at: Option<Instant>,
    /// Shared teardown trace: `"raise"` / `"join"`, in the order they happened, across ALL engines.
    trace: Arc<Mutex<Vec<&'static str>>>,
}

impl ExecutionClient for WindDownClient {
    fn submit(&mut self, _request: &OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}

    fn begin_detach(&mut self) {
        self.trace.lock().expect("trace").push("raise");
        self.raised_at.get_or_insert_with(Instant::now);
    }

    fn detach(&mut self) {
        self.trace.lock().expect("trace").push("join");
        // A detach on a client whose flag was never raised waits the FULL wind-down starting now —
        // exactly what the one-loop teardown paid, once per engine.
        let raised = *self.raised_at.get_or_insert_with(Instant::now);
        let ready = raised + WIND_DOWN;
        let now = Instant::now();
        if now < ready {
            std::thread::sleep(ready - now);
        }
    }
}

fn engine(venue: &str, trace: &Arc<Mutex<Vec<&'static str>>>) -> ExecutionEngine<DynClient> {
    let client: DynClient = Box::new(WindDownClient { raised_at: None, trace: Arc::clone(trace) });
    ExecutionEngine::new(
        Account::new(1.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        venue,
        "BTCUSDT",
    )
}

/// ⚠ **The teardown-cost finding, as a measurement.** Six venues on one core: the stop must cost
/// about ONE wind-down, not six.
#[test]
fn teardown_raises_every_engines_flag_before_it_joins_anything() {
    let trace: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let primary = engine("venue-0", &trace);
    let extras: Vec<(f64, ExecutionEngine<DynClient>)> =
        (1..ENGINES).map(|i| (100.0, engine(&format!("venue-{i}"), &trace))).collect();

    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core_multi(primary, extras, config);

    let started = Instant::now();
    handle.shutdown_and_join();
    let elapsed = started.elapsed();

    let trace = trace.lock().expect("trace");
    assert_eq!(
        trace.iter().filter(|s| **s == "raise").count(),
        ENGINES,
        "every engine's flag must be raised: {trace:?}"
    );
    assert_eq!(
        trace.iter().filter(|s| **s == "join").count(),
        ENGINES,
        "…and every engine must still be detached: {trace:?}"
    );

    // The ordering is the property; the clock is the consequence. Every raise must precede every
    // join, ACROSS engines — a per-engine raise-then-join would give the parallelism back one venue
    // at a time, and this assertion is what notices.
    let first_join = trace.iter().position(|s| *s == "join").expect("joins must have happened");
    let last_raise = trace.iter().rposition(|s| *s == "raise").expect("raises must have happened");
    assert!(
        last_raise < first_join,
        "every engine's stop flag must be RAISED before the first join — trace: {trace:?}"
    );

    // The clock, as a CONSEQUENCE check with the widest headroom the gate can afford. The ceiling
    // is set by what it must REFUSE — the serial teardown costs `ENGINES` wind-downs, so anything
    // under `ENGINES - 1` of them cannot be that — rather than by what a green run should cost
    // (about one). That hands a stalled runner four whole wind-downs (800 ms) of slack before a
    // false red, which the recorder twin's tighter 3× ratio
    // (`crates/vike-recorder/src/runtime.rs`'s `stop_all_raises_every_flag_before_it_joins_anything`)
    // did not need: that test is single-threaded and in-process, while this one crosses a thread
    // boundary through `spawn_core_multi` + `shutdown_and_join` and inherits whatever the box is
    // doing. The ordering assertion above is the gate; if this one ever reddens under load, widen
    // or drop THIS one, never that one.
    let serial_floor = WIND_DOWN * (ENGINES as u32 - 1);
    assert!(
        elapsed < serial_floor,
        "{ENGINES} venues must cost about ONE wind-down ({WIND_DOWN:?}), not {ENGINES}: took \
         {elapsed:?}, which is at least {serial_floor:?}. In production that poll is 1 s per \
         venue, and the difference is whether a daemon's stop fits inside its unit's \
         TimeoutStopSec= before SIGKILL cuts the final journal checkpoint."
    );
    assert!(
        elapsed >= WIND_DOWN,
        "…and the double must actually have simulated a wind-down: took {elapsed:?}"
    );
}
