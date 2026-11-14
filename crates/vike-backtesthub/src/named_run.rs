//! **The NAMED RUN, server side** — one strategy the server already holds, one param set, one
//! window, one pass (`docs/decisions/0064-a-named-run-carries-no-source.md`).
//!
//! # What it answers now
//!
//! ⚠ **No strategy is compiled into this server (plan 2b), so an armed lane resolves NO name.** The
//! resolver this path called (`vike-user-strategies`' `named_run::resolve`) was deleted with every
//! compiled strategy: a strategy is a FILE, run from a profile. [`serve_named_run`] answers
//! `Refused(UnknownStrategy)` with an empty roster once the request is valid, the lane armed and a
//! slot taken, and [`named_roster`] names nothing, armed or not. The wire is unchanged.
//!
//! # The fence
//!
//! 0064's decision 2 rules that a named run's source refusal is a **dependency closure, not a
//! filter**: this path holds no reader of a params table at all now. And the table cannot carry a
//! script anyway, which is the structural half: `NamedRunSpec::params` is
//! `Vec<(String, NamedRunParam)>`, `NamedRunParam` is `i64`/`f64`/`bool`, and `validate_named_run` refuses
//! the reserved `src` key by name.
//!
//! # What it deliberately does NOT do
//!
//! * **It builds no `vike_backtest::harness::ProfileBacktest`.** A profile is TOML text with an
//!   unconstrained `[strategy.params]` value; parsing one here would put a string door back on a
//!   path whose whole claim is that it has none.
//! * **It writes nothing.** 0064's decision 4: a run directory is durable shared state on the
//!   operator's disk under a runs root nothing prunes, so persisting a run stays
//!   `Request::RunStudy`, which is Control.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_backtesthub_client::named_run::{
    NamedRunOutcome, NamedRunRefusal, NamedRunRoster, NamedRunSpec, validate_named_run,
};
use vike_data::HistoryStore;

/// The command-line flag that ARMS the named-run lane on a backtesthub
/// (`vike-backend backtesthub --addr --named-run`). Default OFF.
///
/// ⚠ **An argument, not a setting:** arming the lane is how THIS daemon process is started, the
/// shape a unit's `ExecStart=` states (decision 0111 retired its environment variable).
///
/// ⚠ **Runtime rather than a build feature, and that is 0064's decision 8 leg 2:**
/// `docs/decisions/0035-the-image-ships-every-feature-and-may-be-the-primary-install.md` means a
/// feature gate would be ON by default exactly where it matters most. The other two legs: a run is
/// by orders of magnitude the most expensive per-request lane on this wire, and arming it PUBLISHES
/// the operator's own compiled-in strategy names (0064's decision 7).
pub const NAMED_RUN_FLAG: &str = "--named-run";

/// How many named runs this process will have in flight at once — 0064's decision 3, bound 5.
///
/// **Two**, and the derivation is the unit rather than a throughput target:
/// `deploy/vike-backtesthub.service` bounds this daemon with `MemoryMax`, which is a KILL and not a
/// throttle, so the slot count's job is to keep the concurrent peak of a bounded-window engine run
/// well inside that ceiling rather than to schedule work. One named run holds at most
/// `NAMED_RUN_MAX_BARS` bars plus the engine state over them; two is a modest multiple of that and
/// still leaves the box's own Control-scope traffic — a profile sweep, a study — the room it
/// already had.
///
/// ⚠ **A request arriving with no slot is REFUSED, never QUEUED.** A queue is a connection thread
/// parked for an unbounded time, which is the denial of service this verb's whole classification
/// turns on wearing a different noun; and `crates/vike-hub-protocol/src/limits.rs`'s `SERVER_IDLE_READ_TIMEOUT` disclaims
/// the job of bounding it (*"It bounds the gap between FRAMES, never the length of a RUN"*).
pub const NAMED_RUN_MAX_CONCURRENT: usize = 2;

/// Named runs in flight, process-wide. Shared by every connection thread, which is the point: the
/// ceiling is on the BOX, not on a peer, so N connections cannot each take N slots.
static NAMED_RUNS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// An acquired run slot, released on drop.
///
/// RAII rather than a matched release, deliberately: the run between acquire and release calls into
/// the engine, and an early return or a panic on that path would otherwise leak a slot permanently
/// — after [`NAMED_RUN_MAX_CONCURRENT`] such leaks the lane is closed for the life of the process
/// with no way to tell from outside.
struct NamedRunSlot;

impl NamedRunSlot {
    /// Take a slot, or `None` when all [`NAMED_RUN_MAX_CONCURRENT`] are busy.
    ///
    /// A compare-exchange loop rather than `fetch_add` + compensating `fetch_sub`: the latter lets
    /// the counter transiently exceed the ceiling, and a second thread reading it in that window
    /// refuses a run that was never actually admitted.
    fn acquire() -> Option<Self> {
        let mut seen = NAMED_RUNS_IN_FLIGHT.load(Ordering::Acquire);
        loop {
            if seen >= NAMED_RUN_MAX_CONCURRENT {
                return None;
            }
            match NAMED_RUNS_IN_FLIGHT.compare_exchange_weak(
                seen,
                seen + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(NamedRunSlot),
                Err(actual) => seen = actual,
            }
        }
    }
}

impl Drop for NamedRunSlot {
    fn drop(&mut self) {
        NAMED_RUNS_IN_FLIGHT.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Whether this daemon's operator armed the named-run lane.
///
/// A TYPE rather than a bare `bool` threaded through four `serve*` signatures: at a call site
/// `serve_authed(listener, store, studio, study, keys, true)` says nothing, and the one thing a
/// reader needs to know about that argument is that `false` is the default and the guarded state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NamedRunLane {
    armed: bool,
}

impl NamedRunLane {
    /// The DISARMED lane — what every entry point that takes no configuration passes, and what a
    /// daemon started without [`NAMED_RUN_FLAG`] runs.
    pub const DISARMED: Self = NamedRunLane { armed: false };

    /// Resolve the lane out of the daemon's command line: armed exactly when [`NAMED_RUN_FLAG`] is
    /// one of its arguments, spelled exactly — no value, no `=` form, nothing fuzzy.
    pub fn from_args(args: &[String]) -> Self {
        NamedRunLane { armed: args.iter().any(|a| a == NAMED_RUN_FLAG) }
    }

    /// The ARMED lane — for a test or an embedding that starts a server with the lane on.
    pub const ARMED: Self = NamedRunLane { armed: true };

    /// Whether a run will actually happen. An UNARMED lane still ANSWERS both verbs — 0064's
    /// decision 8 — so this is consulted, never used to refuse the frame.
    pub fn armed(self) -> bool {
        self.armed
    }
}

/// Answer `Request::NamedStrategies`: the roster a named run would resolve, plus the arming.
///
/// ⚠ **Not the folder scan `Request::ListStrategies` answers.** That roster is the daemon's strategy
/// FILES; this one is the names a named run resolves, and since plan 2b it resolves none: no
/// strategy is compiled into this server, so an ARMED lane answers an empty roster too (0064's
/// decision 7: *the roster the verb SERVES is the roster it ENUMERATES*).
/// ⚠ **An UNARMED lane answers an EMPTY roster** as well, 0064's decision 8 leg 3. The verb is still
/// ANSWERED — `armed: false` plus `NamedRunRoster::unarmed_note` is a teaching refusal, not an empty
/// answer.
pub fn named_roster(lane: NamedRunLane) -> NamedRunRoster {
    NamedRunRoster { armed: lane.armed(), strategies: Vec::new() }
}

/// The outcome of a named run, or the message for a `Response::Error`.
///
/// `Err` is reserved for a request that is malformed or a store that failed — the two things that
/// are not an OUTCOME of running. `Ok(NotArmed)` and `Ok(Refused(..))` are successes carrying a
/// fact about the server, which is the `vike_datahub_client::catalog::CatalogOutcome` shape.
pub type NamedRunAnswer = Result<NamedRunOutcome, String>;

/// **Serve one `Request::RunNamed`.**
///
/// The order is deliberate and each step is cheaper than the next:
///
/// 1. **`validate_named_run`** — every bound, re-checked here whatever the client did. The client
///    calls the same function before it writes a frame, but that copy is for the MESSAGE; this one
///    is the enforcement, and it runs even on an UNARMED lane so a caller developing against one
///    still learns its request shape is wrong.
/// 2. **the arming** — 0064's decision 8. An unarmed daemon answers `NotArmed` and touches nothing.
/// 3. **a run slot** — refused, never queued.
/// 4. **the strategy**: no name resolves (this module's doc), so the answer is `UnknownStrategy`
///    with the (empty) roster attached. The store is never read.
pub fn serve_named_run(
    spec: &NamedRunSpec,
    _store: &Arc<dyn HistoryStore + Send + Sync>,
    lane: NamedRunLane,
) -> NamedRunAnswer {
    validate_named_run(spec)?;
    if !lane.armed() {
        return Ok(NamedRunOutcome::NotArmed);
    }
    let Some(_slot) = NamedRunSlot::acquire() else {
        return Ok(NamedRunOutcome::Refused(NamedRunRefusal::NoSlot {
            limit: NAMED_RUN_MAX_CONCURRENT,
        }));
    };
    Ok(NamedRunOutcome::Refused(NamedRunRefusal::UnknownStrategy {
        known: named_roster(lane).strategies,
    }))
}

#[path = "named_run_tests.rs"]
#[cfg(test)]
mod named_run_tests;
