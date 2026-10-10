//! **The NAMED RUN, server side** — one strategy the server already holds, one param set, one
//! window, one pass (`docs/decisions/0064-a-named-run-carries-no-source.md`).
//!
//! # What this module is, in one line
//!
//! It is the ONLY run path in this crate that does not call
//! `crate::harness::registry::strategy_by_name`, and that omission is the whole design.
//!
//! # The fence, and why it is not here
//!
//! 0064's decision 2 rules that a named run's source refusal is a **dependency closure, not a
//! filter**. This crate is on the wrong side of that closure — it names `vike-script` (the `"rhai"`
//! arm of `crate::harness::registry::strategy_by_name` compiles an inline `src`, one `match` line
//! away from arms this module must not reach) — so the resolution does not happen here. It happens
//! in `vike_user_strategies::named_run::resolve`, whose crate's whole dependency set is
//! vike-model, vike-strategy, vike-indicators and toml.
//! `crates/vike-ops/tests/architecture/named_run_closure_gate.rs` is the machine check on that, and it is the
//! gate the record declared it owed.
//!
//! So a `src` key arriving in a params table is **UNREAD here, not refused**: this module hands the
//! table to a function whose closure holds no reader for it. That is the strongest form the
//! refusal takes — *refusing a key is a check, having no reader is a fact.*
//!
//! ⚠ **And the params table cannot hold one anyway**, which is the structural half:
//! `NamedRunSpec::params` is `Vec<(String, NamedParam)>` and `NamedParam` is `i64`/`f64`/`bool`.
//! [`params_table`] below is the only place a `toml::Value` is built on this path and it can emit
//! no string at all. There is no TOML text on this path, no profile parse, and nothing a script
//! could ride in on.
//!
//! # What it deliberately does NOT do
//!
//! * **It builds no `crate::harness::BacktestProfile`.** A profile is TOML text with an
//!   unconstrained `[strategy.params]` value; parsing one here would put a string door back on a
//!   path whose whole claim is that it has none. The engine is assembled from the bounded fields
//!   directly, with `EngineParams::default()` — which is also exactly what 0064's decision 3 means
//!   by omitting "the engine's own cost knobs".
//! * **It runs the BAR lane only.** The window ceiling is priced in bars
//!   (`vike_datahub_client::named_run::NAMED_RUN_MAX_BARS`), so a tick lane would be a dimension
//!   the bound cannot see.
//! * **It writes nothing.** 0064's decision 4: a run directory is durable shared state on the
//!   operator's disk under a runs root nothing prunes, so persisting a run stays
//!   `Request::RunStudy`, which is Control. The residue here dies with the request.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use toml::Value;
use vike_data::{HistStore, TsRange};
use vike_datahub_client::named_run::{
    NamedParam, NamedRoster, NamedRunOutcome, NamedRunRefusal, NamedRunSpec, validate_named_run,
};
use vike_user_strategies::named_run::NamedRunError;

use vike_analytics::report::{BacktestReport, periods_per_year_for_interval};
use vike_sim::{EngineParams, SimBroker, StrategyEngine};

/// The environment variable that ARMS the named-run lane on a compute daemon
/// (`vike-backend backtest --addr`). Default OFF.
///
/// ⚠ **Read out of the caller's map, never out of the process** — `crate::backtest_cli`'s `run`
/// already owns the one `std::env::vars()` sweep this binary performs and hands it down, so this
/// stays a `Layer::Injected` row in `vike_ops::settings::SETTINGS` rather than joining
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN`, which is a ratchet that may only
/// shrink.
///
/// ⚠ **Runtime rather than a build feature, and that is 0064's decision 8 leg 2:**
/// `docs/decisions/0035-the-image-ships-every-feature-and-may-be-the-primary-install.md` means a
/// feature gate would be ON by default exactly where it matters most. The other two legs: a run is
/// by orders of magnitude the most expensive per-request lane on this wire, and arming it PUBLISHES
/// the operator's own compiled-in strategy names (0064's decision 7).
pub const NAMED_RUN_ENV: &str = "VIKE_BACKTEST_NAMED_RUN";

/// How many named runs this process will have in flight at once — 0064's decision 3, bound 5.
///
/// **Two**, and the derivation is the unit rather than a throughput target:
/// `deploy/vike-backtest.service` bounds this daemon with `MemoryMax`, which is a KILL and not a
/// throttle, so the slot count's job is to keep the concurrent peak of a bounded-window engine run
/// well inside that ceiling rather than to schedule work. One named run holds at most
/// `NAMED_RUN_MAX_BARS` bars plus the engine state over them; two is a modest multiple of that and
/// still leaves the box's own Control-scope traffic — a profile sweep, a study — the room it
/// already had.
///
/// ⚠ **A request arriving with no slot is REFUSED, never QUEUED.** A queue is a connection thread
/// parked for an unbounded time, which is the denial of service this verb's whole classification
/// turns on wearing a different noun; and `crate::compute_server`'s `IDLE_READ_TIMEOUT` disclaims
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
struct RunSlot;

impl RunSlot {
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
                Ok(_) => return Some(RunSlot),
                Err(actual) => seen = actual,
            }
        }
    }
}

impl Drop for RunSlot {
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
    /// daemon started without [`NAMED_RUN_ENV`] runs.
    pub const DISARMED: Self = NamedRunLane { armed: false };

    /// Resolve the lane out of an already-loaded environment map.
    ///
    /// The EXACT string `"1"`, not a fuzzy truthy parse — the idiom `VIKE_RECONCILE` and
    /// `VIKE_RECONCILE_GENERATE_MISSING` use, and for their reason: a switch that arms a lane on
    /// `"true"`, `"yes"` and `"on"` also arms it on `"0 # disabled"`, and the operator who wrote
    /// that believes they turned it off.
    pub fn from_vars(vars: &HashMap<String, String>) -> Self {
        NamedRunLane { armed: vars.get(NAMED_RUN_ENV).map(String::as_str) == Some("1") }
    }

    /// Whether a run will actually happen. An UNARMED lane still ANSWERS both verbs — 0064's
    /// decision 8 — so this is consulted, never used to refuse the frame.
    pub fn armed(self) -> bool {
        self.armed
    }
}

/// Answer `Request::NamedStrategies`: the roster a named run would resolve, plus the arming.
///
/// ⚠ **Not `crate::harness::STRATEGIES`.** That roster is what `Request::ListStrategies` answers and
/// it is a different set in BOTH directions — it carries the simulator-only arms that live beside
/// the Rhai compiler and therefore outside the named run's closure, and it has never carried the
/// operator's own compiled-in user strategies, which a named run DOES resolve. 0064's decision 7:
/// *the roster the verb SERVES is the roster it ENUMERATES*, because a client naming into the dark
/// is the empty-answer-versus-refusal lie one layer out.
/// ⚠ **An UNARMED lane answers an EMPTY roster**, and that is 0064's decision 8 leg 3 rather than a
/// shortcut: arming this lane is an operator act partly BECAUSE it publishes the operator's own
/// compiled-in strategy names, and a server that named them while unarmed would make shipping the
/// version perform that disclosure. The verb is still ANSWERED — `armed: false` plus
/// `NamedRoster::unarmed_note` is a teaching refusal, not an empty answer.
pub fn named_roster(lane: NamedRunLane) -> NamedRoster {
    if !lane.armed() {
        return NamedRoster { armed: false, strategies: Vec::new() };
    }
    NamedRoster {
        armed: true,
        strategies: vike_user_strategies::named_run::roster()
            .into_iter()
            .map(str::to_string)
            .collect(),
    }
}

/// The knobs, as the `toml::Value` table the resolver's `from_params` readers expect.
///
/// ⚠ **This function is the reason a `src` cannot arrive as a string even by accident.**
/// [`NamedParam`] has no string variant, so there is no arm here that can emit one — the table this
/// builds holds integers, floats and booleans and nothing else. A future `NamedParam::Text` would
/// not merely widen a surface; it would put back, field for field, the
/// `WireSpec::Native { name, params_toml }` door 0064's decision 5 records as described nowhere in
/// this tree.
///
/// A duplicate key keeps the LAST value, which is `toml`'s own table semantics and the answer a
/// caller who sent the same knob twice would get from any other params path.
fn params_table(params: &[(String, NamedParam)]) -> Value {
    let mut table = toml::map::Map::new();
    for (key, value) in params {
        let v = match value {
            NamedParam::Int(i) => Value::Integer(*i),
            NamedParam::Num(x) => Value::Float(*x),
            NamedParam::Flag(b) => Value::Boolean(*b),
        };
        table.insert(key.clone(), v);
    }
    Value::Table(table)
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
/// 4. **the strategy**, through the compiler-free closure. An unknown name comes back with the
///    roster attached so a picker corrects itself without a second round trip.
/// 5. **the bars**, then one pass of the engine.
pub fn serve_named_run(
    spec: &NamedRunSpec,
    store: &Arc<dyn HistStore + Send + Sync>,
    lane: NamedRunLane,
) -> NamedRunAnswer {
    validate_named_run(spec)?;
    if !lane.armed() {
        return Ok(NamedRunOutcome::NotArmed);
    }
    let Some(_slot) = RunSlot::acquire() else {
        return Ok(NamedRunOutcome::Refused(NamedRunRefusal::NoSlot {
            limit: NAMED_RUN_MAX_CONCURRENT,
        }));
    };
    let params = params_table(&spec.params);
    // THE FENCE. `vike-user-strategies` cannot name `vike-script`, so there is no compiler at the
    // end of this call — not a refusal in front of one. See this module's doc and 0064's decision 2.
    let strategy =
        match vike_user_strategies::named_run::resolve::<SimBroker>(&spec.strategy, &params) {
            Ok(s) => s,
            Err(NamedRunError::Unknown(_)) => {
                return Ok(NamedRunOutcome::Refused(NamedRunRefusal::UnknownStrategy {
                    known: named_roster(lane).strategies,
                }));
            }
            Err(NamedRunError::BadParams(msg)) => return Err(msg),
        };
    // Drop the `+ Send` auto-trait: `vike_model` implements `Strategy<B>` for
    // `Box<dyn Strategy<B>>` and not for the `Send` flavour. An unsizing coercion, so this `let` is
    // the whole conversion — no re-box, no allocation. `crate::harness::registry`'s fall-through
    // does the identical thing for the identical reason.
    let strategy: Box<dyn vike_model::Strategy<SimBroker>> = strategy;

    let bars = store
        .load_bars(
            &spec.venue,
            &spec.symbol,
            &spec.interval,
            TsRange { start: Some(spec.start), end: Some(spec.end) },
        )
        .map_err(|e| format!("named run could not read its window: {e}"))?;

    let result =
        StrategyEngine::new(vec![(spec.symbol.clone(), bars)], strategy, EngineParams::default())
            .run();

    let report = BacktestReport::from_result(
        Some(spec.strategy.clone()),
        &result,
        periods_per_year_for_interval(&spec.interval),
    );
    let report_json = serde_json::to_string(&report)
        .map_err(|e| format!("named run report serialize failed: {e}"))?;
    Ok(NamedRunOutcome::Ran {
        result: Box::new(crate::wire_result::to_wire_result(&result)),
        report_json,
    })
}

#[path = "named_run_tests.rs"]
#[cfg(test)]
mod named_run_tests;
