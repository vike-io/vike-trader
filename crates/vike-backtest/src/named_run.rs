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
//! `crates/vike-ops/tests/named_run_closure_gate.rs` is the machine check on that, and it is the
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

use crate::harness::report::{BacktestReport, periods_per_year_for_interval};
use crate::{EngineParams, SimBroker, StrategyEngine};

/// The environment variable that ARMS the named-run lane on a compute daemon
/// (`vike-backend backtest --addr`). Default OFF.
///
/// ⚠ **Read out of the caller's map, never out of the process** — `crate::backtest_cli`'s `run`
/// already owns the one `std::env::vars()` sweep this binary performs and hands it down, so this
/// stays a `Layer::Injected` row in `vike_ops::settings::SETTINGS` rather than joining
/// `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN`, which is a ratchet that may only
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

#[cfg(test)]
mod tests {
    use super::*;
    use vike_data::test_support::MemHistStore;
    use vike_model::Bar;

    fn spec(strategy: &str) -> NamedRunSpec {
        NamedRunSpec {
            strategy: strategy.to_string(),
            params: Vec::new(),
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: "1d".to_string(),
            start: 0,
            end: 86_400_000 * 4,
        }
    }

    fn seeded_store() -> Arc<dyn HistStore + Send + Sync> {
        let store = MemHistStore::default();
        let bars: Vec<Bar> = (0..5i64)
            .map(|i| Bar {
                ts: i * 86_400_000,
                open: 100.0 + i as f64,
                high: 101.0 + i as f64,
                low: 99.0 + i as f64,
                close: 100.5 + i as f64,
                volume: 10.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            })
            .collect();
        store.append_bars("binance", "BTCUSDT", "1d", &bars, None).expect("the double takes bars");
        Arc::new(store)
    }

    fn armed() -> NamedRunLane {
        NamedRunLane::from_vars(&HashMap::from([(NAMED_RUN_ENV.to_string(), "1".to_string())]))
    }

    /// Serializes every test that takes a RUN SLOT.
    ///
    /// ⚠⚠ **This exists because the slot pool is PROCESS-GLOBAL — which is the production design
    /// working correctly, and a test hazard rather than a defect in it.**
    /// [`NAMED_RUNS_IN_FLIGHT`] bounds the BOX, deliberately, so N connections cannot each take N
    /// slots. `cargo test` then runs this module's `#[test]`s as parallel THREADS of one process,
    /// so a sibling test inside `serve_named_run` is holding a real slot while
    /// [`the_slot_ceiling_refuses_and_then_releases`] tries to take them all — and with
    /// [`NAMED_RUN_MAX_CONCURRENT`] at two, one sibling is enough to make the drain fail.
    ///
    /// ⚠ **MEASURED, and it was a FLAKE rather than a clean failure — the worse shape.** It went
    /// green under `cargo nextest`, which gives every test its own PROCESS and therefore its own
    /// counter, and green on a first `cargo test` run; it failed on a later one inside
    /// `scripts/ci_feature_suite.sh`'s `backtest-hist-replay` arm, which runs plain `cargo test`.
    /// A test whose passing depends on which sibling happens to be mid-run is not evidence of
    /// anything, so the fix is here rather than a retry.
    ///
    /// Poisoning is deliberately ABSORBED (`into_inner`): one failing test must report its own
    /// failure, not convert every later one into a poisoned-lock panic that hides it.
    static SLOT_POOL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Take [`SLOT_POOL`] for the duration of a test that consumes slots.
    fn slot_pool_guard() -> std::sync::MutexGuard<'static, ()> {
        SLOT_POOL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// **THE STRUCTURAL PROPERTY, on the server's own path.** The named-run request type carries
    /// `NamedParam`, which has no string variant — so [`params_table`], the ONLY place this path
    /// builds a `toml::Value`, cannot emit a string at all. A `src` is therefore not merely unread
    /// by the resolver: there is no way to spell one in the table that reaches it.
    ///
    /// ⚠ This is the twin of `vike_user_strategies::named_run`'s
    /// `a_param_cannot_reach_a_compiler_through_the_named_run_resolver`, one layer up. That one
    /// proves the CLOSURE holds no reader; this one proves the CARRIER holds no string.
    ///
    /// ⚠⚠ **THE EXHAUSTIVE MATCH BELOW IS THE TEST, and the loop is only its corroboration.** The
    /// first draft of this test had the loop alone — it built three `NamedParam`s and asserted none
    /// of them rendered as a string. That assertion is about the three VALUES it constructed, not
    /// about the TYPE, so it passed unchanged against a mutation that added `NamedParam::Text` and
    /// made `params_table` emit `Value::String` for it: the carrier grew the exact door this test
    /// claims it cannot have, and the test reported a pass. MEASURED, not hypothesised. The arm
    /// list below has no `_`, so a new variant does not slip past — it fails to COMPILE here, which
    /// is the only way a test can pin "this type has no text variant" rather than "these three
    /// values happen not to be text".
    #[test]
    fn no_param_on_this_path_can_be_a_string() {
        let every = [NamedParam::Int(1), NamedParam::Num(2.5), NamedParam::Flag(true)];
        for p in &every {
            match p {
                // ⚠ NO `_` ARM, EVER. See this test's ⚠⚠ above: the exhaustiveness IS the
                // assertion. Adding a variant here to make it compile again is adding the door
                // `docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 2 exists to
                // remove, and it moves `Request::RunNamed` out of `VerbScope::Observe`.
                NamedParam::Int(_) | NamedParam::Num(_) | NamedParam::Flag(_) => {}
            }
        }
        // ...and the corroboration: what `params_table` actually emits for every one of them, with
        // the reserved key's own spelling among them so the rendering is exercised on the exact
        // name a script would arrive under.
        let table = params_table(&[
            (vike_model::RESERVED_SRC_KEY.to_string(), NamedParam::Int(1)),
            ("size".to_string(), NamedParam::Num(2.5)),
            ("live".to_string(), NamedParam::Flag(true)),
        ]);
        let table = table.as_table().expect("params_table builds a table");
        for (key, value) in table {
            assert!(
                value.as_str().is_none(),
                "the params key {key:?} rendered as a STRING, which is the one shape a named run's \
                 carrier must not be able to produce"
            );
        }
    }

    /// A `src` key IS refused, by the belt — and the refusal says what it is and where the script
    /// path actually lives, rather than ignoring the key and running something the caller did not
    /// ask for.
    #[test]
    fn the_reserved_source_key_is_refused_before_the_store_is_touched() {
        let _pool = slot_pool_guard();
        let mut s = spec("buy_hold");
        s.params.push((vike_model::RESERVED_SRC_KEY.to_string(), NamedParam::Int(1)));
        let err = serve_named_run(&s, &seeded_store(), armed())
            .expect_err("a reserved-key request must be an Error, not an outcome");
        assert!(err.contains("carries no source"), "the refusal must say what it is: {err}");
    }

    /// An UNARMED daemon ANSWERS — 0064's decision 8 — and it runs nothing.
    #[test]
    fn an_unarmed_lane_answers_rather_than_erroring() {
        let outcome = serve_named_run(&spec("buy_hold"), &seeded_store(), NamedRunLane::DISARMED)
            .expect("an unarmed lane answers");
        assert_eq!(outcome, NamedRunOutcome::NotArmed);
        let unarmed = named_roster(NamedRunLane::DISARMED);
        assert!(!unarmed.armed);
        // ...and it NAMES NOTHING. 0064's decision 8 leg 3: arming is an operator act partly
        // because it publishes the operator's own compiled-in strategy names, so a server that
        // listed them while unarmed would perform that disclosure by shipping a version. The
        // client still renders `unarmed_note`, which is why this is a refusal and not a silence.
        assert!(
            unarmed.strategies.is_empty(),
            "an UNARMED daemon published its operator's strategy names: {:?}",
            unarmed.strategies
        );
        // ...while the SAME code armed names them, so the emptiness above is the arming and not a
        // roster that is empty for some unrelated reason.
        assert!(!named_roster(armed()).strategies.is_empty());
    }

    /// The bounds are enforced on the SERVER, whatever the client did — and each refusal names the
    /// constant it hit. This is the re-check leg of the negotiation precedent.
    #[test]
    fn the_server_re_checks_every_bound_even_unarmed() {
        let mut wide = spec("buy_hold");
        wide.interval = "1s".to_string();
        wide.end = wide.start + 86_400_000 * 30; // ~2.6M one-second bars
        let err = serve_named_run(&wide, &seeded_store(), NamedRunLane::DISARMED)
            .expect_err("an over-wide window is refused");
        assert!(err.contains("NAMED_RUN_MAX_BARS"), "the refusal must name its bound: {err}");
    }

    /// An unknown name comes back with the roster attached, so a picker can correct itself — and
    /// `"rhai"` lands here like any other name this closure cannot resolve.
    #[test]
    fn an_unknown_name_answers_with_the_roster() {
        let _pool = slot_pool_guard();
        for name in ["rhai", "not_a_strategy"] {
            let outcome = serve_named_run(&spec(name), &seeded_store(), armed())
                .unwrap_or_else(|e| panic!("{name} must be an outcome, not an error: {e}"));
            match outcome {
                NamedRunOutcome::Refused(NamedRunRefusal::UnknownStrategy { known }) => {
                    assert!(!known.is_empty(), "the refusal must carry this server's roster");
                    assert!(!known.iter().any(|k| k == "rhai"));
                }
                other => panic!("{name} resolved to {other:?}"),
            }
        }
    }

    /// The happy path: an armed lane runs one pass and answers with BOTH the curve and the report,
    /// the same pair a `RunSlice` answers with.
    #[test]
    fn an_armed_lane_runs_one_pass_and_answers_with_a_curve_and_a_report() {
        let _pool = slot_pool_guard();
        let outcome =
            serve_named_run(&spec("buy_hold"), &seeded_store(), armed()).expect("the run answers");
        match outcome {
            NamedRunOutcome::Ran { result, report_json } => {
                assert!(!result.equity_curve.is_empty(), "the run produced no equity curve");
                assert_eq!(result.equity_curve.len(), result.equity_ts.len());
                let report: serde_json::Value =
                    serde_json::from_str(&report_json).expect("the report is JSON");
                assert_eq!(report["name"], "buy_hold");
            }
            other => panic!("expected a run, got {other:?}"),
        }
    }

    /// The slot ceiling refuses rather than queueing, and the guard RELEASES — a leak would close
    /// the lane for the life of the process with nothing visible from outside.
    #[test]
    fn the_slot_ceiling_refuses_and_then_releases() {
        let _pool = slot_pool_guard();
        let held: Vec<RunSlot> = (0..NAMED_RUN_MAX_CONCURRENT)
            .map(|_| RunSlot::acquire().expect("a free slot"))
            .collect();
        assert!(RunSlot::acquire().is_none(), "the ceiling admitted one too many");
        let outcome = serve_named_run(&spec("buy_hold"), &seeded_store(), armed())
            .expect("a full lane answers rather than erroring");
        assert_eq!(
            outcome,
            NamedRunOutcome::Refused(NamedRunRefusal::NoSlot { limit: NAMED_RUN_MAX_CONCURRENT })
        );
        drop(held);
        assert!(RunSlot::acquire().is_some(), "the slot guard leaked on drop");
    }

    /// The switch is the EXACT string `"1"`, the `VIKE_RECONCILE` idiom — so `"true"` and
    /// `"0 # off"` both leave the lane disarmed rather than arming it by accident.
    #[test]
    fn only_the_exact_string_one_arms_the_lane() {
        for (value, want) in
            [("1", true), ("true", false), ("0", false), ("", false), (" 1", false)]
        {
            let vars = HashMap::from([(NAMED_RUN_ENV.to_string(), value.to_string())]);
            assert_eq!(NamedRunLane::from_vars(&vars).armed(), want, "value {value:?}");
        }
        assert!(!NamedRunLane::from_vars(&HashMap::new()).armed(), "the default is DISARMED");
    }
}
