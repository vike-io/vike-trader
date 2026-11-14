use super::*;
use vike_backtesthub_client::named_run::NamedRunParam;
use vike_data::HistoryStoreMemory;
use vike_marketdata::Bar;

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

fn seeded_store() -> Arc<dyn HistoryStore + Send + Sync> {
    let store = HistoryStoreMemory::default();
    let bars: Vec<Bar> = (0..5i64)
        .map(|i| Bar {
            timestamp: i * 86_400_000,
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
    NamedRunLane::from_args(&[NAMED_RUN_FLAG.to_string()])
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
/// `NamedRunParam`, which has no string variant, so there is no way to spell a `src` in a named run's
/// params at all.
///
/// ⚠⚠ **THE EXHAUSTIVE MATCH BELOW IS THE TEST.** A loop over three constructed `NamedRunParam`s is
/// about those VALUES, not about the TYPE, so it passed unchanged against a mutation that added
/// `NamedRunParam::Text` (MEASURED, not hypothesised). The arm list below has no `_`, so a new variant
/// does not slip past — it fails to COMPILE here, which is the only way a test can pin "this type
/// has no text variant" rather than "these three values happen not to be text".
#[test]
fn no_param_on_this_path_can_be_a_string() {
    let every = [NamedRunParam::Int(1), NamedRunParam::Num(2.5), NamedRunParam::Flag(true)];
    for p in &every {
        match p {
            // ⚠ NO `_` ARM, EVER. See this test's ⚠⚠ above: the exhaustiveness IS the
            // assertion. Adding a variant here to make it compile again is adding the door
            // `docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 2 exists to
            // remove, and it moves `Request::RunNamed` out of `VerbScope::Read`.
            NamedRunParam::Int(_) | NamedRunParam::Num(_) | NamedRunParam::Flag(_) => {}
        }
    }
}

/// A `src` key IS refused, by the belt — and the refusal says what it is and where the script
/// path actually lives, rather than ignoring the key and running something the caller did not
/// ask for.
#[test]
fn the_reserved_source_key_is_refused_before_the_store_is_touched() {
    let _pool = slot_pool_guard();
    let mut s = spec("buy_hold");
    s.params.push((vike_model::RESERVED_SOURCE_KEY.to_string(), NamedRunParam::Int(1)));
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

/// **An armed lane resolves NO name** (plan 2b: no strategy is compiled into this server): every
/// name, a shipped file's stem and `"rhai"` among them, comes back `UnknownStrategy` with the
/// (empty) roster attached, and the armed roster names nothing either.
#[test]
fn an_armed_lane_resolves_no_name() {
    let _pool = slot_pool_guard();
    for name in ["buy_hold", "rhai", "not_a_strategy"] {
        let outcome = serve_named_run(&spec(name), &seeded_store(), armed())
            .unwrap_or_else(|e| panic!("{name} must be an outcome, not an error: {e}"));
        assert_eq!(
            outcome,
            NamedRunOutcome::Refused(NamedRunRefusal::UnknownStrategy { known: Vec::new() }),
            "{name}"
        );
    }
    let roster = named_roster(armed());
    assert!(roster.armed && roster.strategies.is_empty(), "{roster:?}");
}

/// The slot ceiling refuses rather than queueing, and the guard RELEASES — a leak would close
/// the lane for the life of the process with nothing visible from outside.
#[test]
fn the_slot_ceiling_refuses_and_then_releases() {
    let _pool = slot_pool_guard();
    let held: Vec<NamedRunSlot> = (0..NAMED_RUN_MAX_CONCURRENT)
        .map(|_| NamedRunSlot::acquire().expect("a free slot"))
        .collect();
    assert!(NamedRunSlot::acquire().is_none(), "the ceiling admitted one too many");
    let outcome = serve_named_run(&spec("buy_hold"), &seeded_store(), armed())
        .expect("a full lane answers rather than erroring");
    assert_eq!(
        outcome,
        NamedRunOutcome::Refused(NamedRunRefusal::NoSlot { limit: NAMED_RUN_MAX_CONCURRENT })
    );
    drop(held);
    assert!(NamedRunSlot::acquire().is_some(), "the slot guard leaked on drop");
}

/// The switch is the EXACT flag — so a near spelling, an `=` form or a value leaves the lane
/// disarmed rather than arming it by accident.
#[test]
fn only_the_exact_flag_arms_the_lane() {
    for (arg, want) in [
        ("--named-run", true),
        ("--named-run=1", false),
        ("--named-runs", false),
        ("named-run", false),
        ("1", false),
    ] {
        let line = ["--addr".to_string(), arg.to_string()];
        assert_eq!(NamedRunLane::from_args(&line).armed(), want, "argument {arg:?}");
    }
    assert!(!NamedRunLane::from_args(&["--addr".to_string()]).armed(), "the default is DISARMED");
    assert_eq!(NamedRunLane::ARMED, armed());
}
