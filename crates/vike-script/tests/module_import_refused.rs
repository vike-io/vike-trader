//! ⚠ **THE enforcement gate for this crate's two script engines.**
//!
//! `rhai::Engine::new()` installs a `FileModuleResolver` rooted at the process's working directory
//! (rhai-1.25.1 `src/engine.rs`'s `new`), so on a DEFAULT engine `import "…" as m;` READS A FILE
//! FROM DISK. That is a capability nobody granted: it appears in no registration list, because it
//! is the interpreter's default rather than anything this host handed over — which is exactly the
//! kind of surface an allow-list cannot see. `crates/vike-script/src/engine.rs`'s `build_engine`
//! (strategies) and `crates/vike-script/src/indicator.rs`'s `indicator_engine` (user indicators)
//! each replace it with `rhai::module_resolvers::DummyModuleResolver`; these tests keep those two
//! lines from being tidied away by somebody who reads them as boilerplate.
//!
//! ⚠ **Every refusal assertion here is paired with a CONTROL, because the obvious version of this
//! test is VACUOUS.** `import "neighbour"` fails under BOTH resolvers — a test that only asserts
//! "the import errored" would go on passing after somebody deleted the very line it exists to
//! protect. So each test below plants a module a `FileModuleResolver` genuinely WOULD load and
//! asserts a default engine loads it, and the two behaviour tests additionally assert that the
//! SAME script with the `import` removed does the thing the import case does not — which is what
//! rules out "the script was broken for some other reason".
//!
//! The study tier closed this hazard first and is the model:
//! `crates/vike-studio-core/tests/rhai_study_pipeline.rs`'s
//! `a_study_cannot_import_a_module_because_that_would_be_a_file_verb`.

use std::path::{Path, PathBuf};
use vike_model::{Bar, Broker, Strategy};
use vike_script::{compile_indicator, Indicator, RhaiStrategy};

/// A throwaway directory, hand-rolled rather than pulled from `tempfile`: this crate has no
/// dev-dependency on it, and the shape mirrors `install_from_user_data.rs`'s own `Scratch`.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("vike-script-import-{tag}-{nanos}"));
        std::fs::create_dir_all(&p).expect("scratch");
        Self(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Plants `helper.rhai` in a fresh scratch directory and returns the module path to `import`, ALONG
/// WITH the proof that a default engine really does load it.
///
/// ⚠ The path is ABSOLUTE so the answer cannot depend on the process's working directory, which
/// every test in one binary shares. Forward slashes: a Windows `\` is an ESCAPE inside a rhai
/// string literal, and `Path::join` takes `/` on every platform this ships to.
///
/// The CONTROL assertion lives here rather than in a comment: if a default rhai engine ever stops
/// reading this file off disk, the hazard is gone and every test in this file should be re-argued.
fn plant_module(tag: &str) -> (Scratch, String) {
    let scratch = Scratch::new(tag);
    std::fs::write(scratch.path().join("helper.rhai"), "fn answer() { 42 }\n").unwrap();
    let module = scratch.path().join("helper").display().to_string().replace('\\', "/");

    let answer: i64 = rhai::Engine::new()
        .eval(&format!(r#"import "{module}" as m; m::answer()"#))
        .expect("a DEFAULT rhai engine resolves a module from the filesystem");
    assert_eq!(answer, 42, "the planted module is one a FileModuleResolver really does load");

    (scratch, module)
}

// -------------------------------------------------------------------------------------------
// the strategy engine (`engine.rs`'s `build_engine`)
// -------------------------------------------------------------------------------------------

/// A strategy's TOP LEVEL is where an author would write an import, and it is run exactly once by
/// `compile` — so the refusal surfaces as a compile error rather than as a surprise on some later
/// bar.
#[test]
fn a_strategy_cannot_import_a_module_because_that_would_be_a_file_verb() {
    let (_scratch, module) = plant_module("strategy-top");

    // `match` rather than `expect_err`: `RhaiStrategy` implements no `Debug`, which that method
    // requires of the Ok side.
    let e = match RhaiStrategy::<MockBroker>::compile(&format!(
        r#"import "{module}" as m; fn on_bar() {{ buy(1.0); }}"#
    )) {
        Ok(_) => panic!("an import must not resolve in a strategy"),
        Err(e) => e,
    };
    assert!(
        e.to_string().to_lowercase().contains("module"),
        "the refusal should name what was refused: {e}"
    );

    // The CONTROL for "the script was otherwise fine": the same source WITHOUT the import compiles.
    RhaiStrategy::<MockBroker>::compile("fn on_bar() { buy(1.0); }")
        .expect("the same script without the import is valid");
}

/// ...and inside a hook, which the compile-time top-level run never touches.
///
/// `run_hook` fails SAFE rather than propagating (it discards the bar's intents and warns), so the
/// observable is the ORDER THAT NEVER LANDS. Both halves are asserted: the importing script places
/// nothing, and the identical script without the import places the order — so an empty `submits`
/// cannot be explained by the strategy failing to mount.
#[test]
fn a_strategy_cannot_import_a_module_from_inside_a_hook_either() {
    let (_scratch, module) = plant_module("strategy-hook");

    let mut importer = RhaiStrategy::<MockBroker>::compile(&format!(
        r#"fn on_bar() {{ import "{module}" as m; buy(1.0); }}"#
    ))
    .expect("the import is inside a fn, so compile's top-level run never reaches it");
    let mut b = MockBroker::default();
    importer.on_bar(&mut b, &bar(100.0));
    assert!(
        b.submits.is_empty(),
        "the hook must have failed on the import, before `buy` could reach the broker: {:?}",
        b.submits
    );

    // The CONTROL: identical hook, no import — the order lands. Without this assertion an empty
    // `submits` above would also be produced by a strategy that never ran at all.
    let mut plain =
        RhaiStrategy::<MockBroker>::compile("fn on_bar() { buy(1.0); }").expect("compiles");
    let mut b2 = MockBroker::default();
    plain.on_bar(&mut b2, &bar(100.0));
    assert_eq!(b2.submits, vec![(1, 1.0)], "the control script does place its order");
}

// -------------------------------------------------------------------------------------------
// the user-indicator engine (`indicator.rs`'s `indicator_engine`)
// -------------------------------------------------------------------------------------------

/// The same hazard on the seam that loads the LEAST reviewed scripts in the tree: a user indicator
/// is picked up from `user_data/indicators/`, not named in a profile.
#[test]
fn a_user_indicator_cannot_import_a_module_because_that_would_be_a_file_verb() {
    let (_scratch, module) = plant_module("indicator-top");

    let e = compile_indicator(
        "importer",
        &format!(r#"import "{module}" as m; fn on_bar(bar) {{ bar.close }}"#),
    )
    .expect_err("an import must not resolve in a user indicator");
    assert!(e.to_string().contains("importer"), "the refusal names the indicator: {e}");

    // The CONTROL for "the source was otherwise fine".
    compile_indicator("plain", "fn on_bar(bar) { bar.close }")
        .expect("the same source without the import is valid");
}

/// ...and inside `on_bar`, where the indicator tier HOLDS the fault rather than propagating it.
#[test]
fn a_user_indicator_cannot_import_a_module_from_inside_on_bar_either() {
    let (_scratch, module) = plant_module("indicator-bar");

    let mut ind = compile_indicator(
        "late",
        &format!(r#"fn on_bar(bar) {{ import "{module}" as m; m::answer() }}"#),
    )
    .expect("the import is inside a fn, so the top-level run never reaches it");
    let out = ind.on_bar(&bar(100.0));
    // `!is_empty()` first: `all()` over an empty vector is vacuously true, which would make the
    // NaN claim below pass for an indicator that returned no lines at all.
    assert!(!out.is_empty(), "an indicator always returns one value per declared line");
    assert!(out.iter().all(|v| v.is_nan()), "a faulted bar yields NaN, got {out:?}");
    let fault = ind.fault().expect("the refusal is HELD as a fault");
    assert!(fault.to_lowercase().contains("module"), "the fault names what was refused: {fault}");

    // The CONTROL: identical shape, no import — a real value comes back and no fault is held.
    let mut plain = compile_indicator("plain", "fn on_bar(bar) { 42.0 }").expect("compiles");
    assert_eq!(plain.on_bar(&bar(100.0)), vec![42.0]);
    assert!(plain.fault().is_none(), "the control holds no fault: {:?}", plain.fault());
}

// -------------------------------------------------------------------------------------------
// fixtures
// -------------------------------------------------------------------------------------------

fn bar(c: f64) -> Bar {
    Bar {
        ts: 0,
        open: c,
        high: c,
        low: c,
        close: c,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: Some("X".into()),
    }
}

/// The same minimal broker `binding.rs` uses; duplicated rather than shared because cargo compiles
/// each `tests/*.rs` as its own crate.
#[derive(Default)]
struct MockBroker {
    submits: Vec<(i32, f64)>,
    bars: Vec<Bar>,
}

impl Broker for MockBroker {
    fn submit_market(&mut self, _s: &str, side: i32, qty: f64) {
        self.submits.push((side, qty));
    }
    fn submit_limit(&mut self, _s: &str, _side: i32, _qty: f64, _p: f64) {}
    fn position(&self, _s: &str) -> f64 {
        0.0
    }
    fn price(&self, _s: &str) -> f64 {
        self.bars.last().map(|b| b.close).unwrap_or(0.0)
    }
    fn equity(&self) -> f64 {
        10_000.0
    }
    fn bars(&self, _s: &str) -> &[Bar] {
        &self.bars
    }
    fn index(&self) -> usize {
        self.bars.len().saturating_sub(1)
    }
    fn now(&self) -> i64 {
        0
    }
}
