use super::*;
use vike_data::MemHistStore;
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
            // remove, and it moves `Request::RunNamed` out of `VerbScope::Read`.
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
    let held: Vec<RunSlot> =
        (0..NAMED_RUN_MAX_CONCURRENT).map(|_| RunSlot::acquire().expect("a free slot")).collect();
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
