//! ⚠ **The gate behind "a starter template actually trades."** Every template in
//! [`vike_studio_core::templates`] is EXECUTED here — through the crate's own production
//! [`vike_studio_core::run_slice`] pipeline, over a real store — and must produce a closed trade.
//!
//! # Why a compile check is the wrong bar
//!
//! `templates.rs`'s in-crate test (and `vike-cli`'s hand copy of the same sources) pins that each
//! template COMPILES and declares `param()` knobs. Neither can see the property that matters,
//! because of how Rhai resolves names: a REGISTERED function is looked up when its line RUNS, not
//! when the script is parsed, and the compile path
//! (`RhaiStrategy::compile_with_params` / `discover_params`) runs the script's TOP LEVEL exactly
//! once. Every call a template makes lives inside `fn on_bar()`, which that one-time run never
//! enters — `AST::iter_functions` merely notes the function exists.
//!
//! So three distinct mistakes land in the same blind spot, indistinguishably:
//!
//! - an unbound NAME — a plain typo, or any name outside `vike_script::RHAI_INDICATORS`, the set
//!   `crates/vike-script/src/engine.rs`'s `register_indicators` actually registers;
//! - a wrong ARITY;
//! - a wrong ARGUMENT TYPE — `rhai`'s `resolve_fn` hashes each argument's `TypeId` and performs no
//!   INT->FLOAT coercion for a registered function, so `market(1, 1)` misses exactly as hard as a
//!   misspelling does.
//!
//! Each raises `ErrorFunctionNotFound` on every bar. `RhaiStrategy`'s hook runner is deliberately
//! FAIL-SAFE: it drops that bar's intents, logs, and self-disables after ten consecutive errors —
//! no `Result` reaches the caller. The strategy looks mounted and silently never trades, which a
//! backtest reports as a flat curve, i.e. as "no signal". Only execution tells the two apart.
//!
//! # Why it lives in `tests/` rather than beside the sources
//!
//! It needs `tempfile` (a dev-dependency) for the throwaway store, and it drives the whole public
//! Run pipeline rather than `templates.rs`'s internals — an integration test is the honest shape
//! for both. It deliberately does NOT hand-drive a broker double: `vike_model::MockBroker` is
//! behind that crate's `test-support` feature, which this crate does not take, and re-declaring a
//! local `Broker` impl is exactly the per-crate duplication that double was consolidated to end.
//! Going through `run_slice` is stronger anyway — it proves the orders FILL through the real
//! engine, not merely that they were submitted.

use std::sync::Arc;

use vike_backtest::EngineParams;
use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_model::Bar;
use vike_studio_core::{DataSlice, StoreHandle, StrategySpec, run_slice, templates};

/// A function name NOTHING binds — the specimen `an_unbound_call_compiles_and_then_trades_nothing`
/// rewrites a template to call. Typo-shaped on purpose: the failure it stands for is a misspelling,
/// and the assertion at the call site proves it is unbound rather than trusting this comment.
const UNBOUND_WITNESS: &str = "sma_typo";

/// Half a cycle of [`triangle_bars`], in bars. Comfortably longer than the slowest lookback any
/// template uses (`sma(20)`, `rsi(14)`), so each leg fully re-saturates its indicators.
const LEG: usize = 20;
/// Bar-to-bar move of the triangle wave, in price units.
const STEP: f64 = 2.0;
/// Full up-down cycles seeded into the store — ten of them, so every template gets many chances to
/// close a round trip and a failure cannot be read as "it just needed a longer series".
const CYCLES: usize = 10;

/// A deterministic TRIANGLE wave: `LEG` strictly rising bars, then `LEG` strictly falling ones,
/// repeated. Chosen over a random walk (which `run.rs`'s own tests use) because this gate asserts
/// something about EVERY template, and each needs a different thing from the series:
///
/// - the SMA cross needs `sma(5)` to cross `sma(20)` in both directions — a wave does that once
///   per leg;
/// - the RSI reversion needs `rsi(14)` to leave BOTH ends of its 30/70 band, which a run of `LEG`
///   consecutive same-signed moves guarantees outright (a run of >= 14 rises pins RSI at 100, a
///   run of >= 14 falls pins it at 0) rather than probably;
/// - the breakout needs price on both sides of its channel.
///
/// A random walk supplies all three only with high probability. This supplies them by
/// construction, so a failure here is always the template's fault and never the fixture's.
fn triangle_bars(cycles: usize) -> Vec<Bar> {
    (0..cycles * LEG * 2)
        .map(|i| {
            let phase = i % (LEG * 2);
            // rise across the first leg, fall across the second — a symmetric peak at `phase == LEG`
            let rungs = if phase < LEG { phase } else { LEG * 2 - phase };
            let px = 100.0 + rungs as f64 * STEP;
            Bar {
                ts: 60_000 * (i as i64 + 1),
                open: px,
                high: px,
                low: px,
                close: px,
                volume: 1.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some("BTCUSDT".into()),
            }
        })
        .collect()
}

/// A throwaway `DataFusionHist` holding [`CYCLES`] triangle cycles of `binance BTCUSDT 1m` bars. The
/// `TempDir` is returned alongside the handle and must be held for the store's lifetime — dropping
/// it deletes the directory out from under an open store.
fn seeded_store() -> (tempfile::TempDir, StoreHandle) {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_bars("binance", "BTCUSDT", "1m", &triangle_bars(CYCLES), None).unwrap();
    (dir, Arc::new(store) as StoreHandle)
}

fn slice() -> DataSlice {
    DataSlice::bars("binance", "BTCUSDT", "1m", TsRange::all())
}

/// Every shipped template, run through the real pipeline, must close at least one trade.
///
/// `n_trades` (not "an order was submitted") is the assertion on purpose: it counts ROUND TRIPS
/// booked by the engine, so it proves the template's orders were accepted AND filled — a strictly
/// stronger statement, and the one a user reading the templates dropdown is implicitly promised.
#[test]
fn every_template_actually_trades_through_the_real_run_pipeline() {
    let (_dir, store) = seeded_store();
    for (name, src) in templates::templates() {
        let res = run_slice(&StrategySpec::rhai(*src), &slice(), &store, EngineParams::default())
            .unwrap_or_else(|e| panic!("template {name} must run: {e}"));
        assert!(
            res.n_trades > 0,
            "template {name} closed no trade over {} bars of a triangle wave that crosses every \
             band it could gate on — a starter a user copies must actually trade. Check that \
             every function it calls is host-bound (`vike_script::RHAI_INDICATORS`, plus \
             `crates/vike-script/src/engine.rs`'s `register_reads`/`register_verbs`/ \
             `build_engine`) and that each call's arity and ARGUMENT TYPES match the registration \
             exactly: rhai resolves a registered function by exact TypeId per argument and \
             coerces neither.",
            LEG * 2 * CYCLES
        );
    }
}

/// The proof that the gate above is not vacuous, and a live specimen of the failure it catches.
///
/// The SMA-cross template with its `sma(` calls rewritten to [`UNBOUND_WITNESS`] still COMPILES and
/// still declares its knobs — so both compile-only gates stay green on it — yet it trades nothing.
/// Without this, `every_template_actually_trades_through_the_real_run_pipeline` would be a claim
/// about the fixture as much as about the templates.
///
/// ⚠ The witness used to be `wma`, a REAL registry indicator the host did not bind. That stopped
/// being true when the host widened to the registry, so the witness is now a typo-shaped name
/// NOTHING binds, asserted against `vike_script::RHAI_INDICATORS` rather than assumed. Do not
/// restore a registry name here: a witness that IS bound makes this test pass for the wrong reason
/// and quietly turns the gate above into a claim about nothing.
#[test]
fn an_unbound_call_compiles_and_then_trades_nothing() {
    let (_dir, store) = seeded_store();
    // Whichever template calls `sma` — found rather than indexed, so reordering the list cannot
    // silently turn this into a test of a template with nothing to rewrite.
    let (name, src) = templates::templates()
        .iter()
        .find(|(_, src)| src.contains("sma("))
        .expect("test premise: some template calls sma()");
    let call = format!("{UNBOUND_WITNESS}(");
    let unbound = src.replace("sma(", &call);
    assert!(unbound.contains(&call), "test premise: {name} must contain the rewritten call");
    assert!(
        vike_script::discover_params(&unbound).is_ok(),
        "test premise: a call to an unbound function still COMPILES — that is the whole hazard"
    );
    assert!(
        !vike_script::RHAI_INDICATORS.contains(&UNBOUND_WITNESS),
        "test premise: the witness must stay outside the host-bound set"
    );

    let res = run_slice(&StrategySpec::rhai(unbound), &slice(), &store, EngineParams::default())
        .expect("an unbound CALL is a run-time error per bar, never a compile error");
    assert_eq!(
        res.n_trades, 0,
        "a script whose indicator call is unbound must trade nothing — otherwise \
         `every_template_actually_trades_through_the_real_run_pipeline` could not detect one"
    );
}
